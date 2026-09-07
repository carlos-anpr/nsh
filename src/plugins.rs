//! Plugins instalables con un comando (`/plugins install <nombre>`).
//!
//! El núcleo de nsh (comandos `!` + LLM) siempre funciona. Los plugins añaden
//! capacidades opcionales. Cada entrada del catálogo describe el bloque
//! `[connectors.*]` exacto que necesita; instalar un plugin es escribir ese
//! bloque en el `config.toml` por el usuario, sin que edite nada a mano.
//!
//! El runtime (`uvx`, `npx`, ...) lo resuelve el propio comando en el primer
//! uso (ejecución efímera, como hacen Claude Code, Gemini CLI y OpenCode):
//! `install` solo comprueba que el runtime exista en el PATH.

use anyhow::{Result, bail};
use std::collections::BTreeMap;
use std::path::Path;

use crate::config::{
    Config, ConnectorApproval, ConnectorConfig, ConnectorToolConfig, ConnectorToolEffect,
    ConnectorWorkingDir, config_path,
};

/// Descripción estática de un plugin instalable.
pub struct PluginSpec {
    /// Nombre para `/plugins install <nombre>`.
    pub name: &'static str,
    /// Una línea para `/plugins list`.
    pub description: &'static str,
    /// Runtime necesario en el PATH (`uvx`, `npx`, ...).
    pub runtime: &'static str,
    /// Cómo conseguir el runtime si falta.
    pub runtime_hint: &'static str,
    /// Clave `[connectors.<connector>]` que escribe.
    pub connector: &'static str,
    /// Tool que usa el pre-paso documental.
    pub tool: &'static str,
    pub command: &'static str,
    pub args: &'static [&'static str],
    pub timeout_ms: u64,
    pub max_output_bytes: usize,
    /// Extensiones (minúsculas, sin punto) que cubre.
    pub extensions: &'static [&'static str],
}

pub const DOCUMENTOS: PluginSpec = PluginSpec {
    name: "documentos",
    description: "lee pdf/docx/xlsx vía MarkItDown",
    runtime: "uvx",
    runtime_hint: "instala uv (que trae uvx) desde https://docs.astral.sh/uv/ y reintenta",
    connector: "markitdown",
    tool: "convert_to_markdown",
    command: "uvx",
    args: &["markitdown-mcp==0.0.1a4"],
    timeout_ms: 120_000,
    max_output_bytes: 1024 * 1024,
    extensions: &["pdf", "docx", "xlsx"],
};

/// Catálogo embebido. Añadir un plugin futuro es añadir una entrada aquí.
pub fn catalog() -> Vec<&'static PluginSpec> {
    vec![&DOCUMENTOS]
}

pub fn find(name: &str) -> Option<&'static PluginSpec> {
    catalog().into_iter().find(|spec| spec.name == name)
}

/// Plugin que sabe convertir la extensión dada (`pdf`, sin punto).
pub fn find_for_extension(ext: &str) -> Option<&'static PluginSpec> {
    let ext = ext.to_ascii_lowercase();
    catalog()
        .into_iter()
        .find(|spec| spec.extensions.contains(&ext.as_str()))
}

#[derive(Debug, PartialEq, Eq)]
pub enum PluginState {
    NotInstalled,
    Disabled,
    Enabled,
}

pub fn state(cfg: &Config, spec: &PluginSpec) -> PluginState {
    match cfg.connectors.get(spec.connector) {
        None => PluginState::NotInstalled,
        Some(connector) if connector.enabled => PluginState::Enabled,
        Some(_) => PluginState::Disabled,
    }
}

fn state_label(state: &PluginState) -> &'static str {
    match state {
        PluginState::Enabled => "instalado",
        PluginState::Disabled => "desactivado",
        PluginState::NotInstalled => "no instalado",
    }
}

/// Líneas para `/plugins list`.
pub fn list_lines(cfg: Option<&Config>) -> Vec<String> {
    let mut lines = vec!["  Plugins disponibles:".to_string()];
    for spec in catalog() {
        let label = cfg
            .map(|c| state_label(&state(c, spec)))
            .unwrap_or("no instalado");
        lines.push(format!(
            "    {:<10} {} ({})  [{}]",
            spec.name, spec.description, spec.runtime, label
        ));
    }
    lines.push("  Uso: /plugins install <nombre>  |  /plugins remove <nombre>".to_string());
    lines
}

/// Comprueba que el runtime del plugin exista en el PATH.
pub fn check_runtime(spec: &PluginSpec) -> Result<()> {
    match std::process::Command::new(spec.runtime)
        .arg("--version")
        .output()
    {
        Ok(out) if out.status.success() => Ok(()),
        _ => bail!(
            "el plugin '{}' necesita `{}` y no está en el PATH. {}.",
            spec.name,
            spec.runtime,
            spec.runtime_hint
        ),
    }
}

fn build_connector(spec: &PluginSpec) -> ConnectorConfig {
    ConnectorConfig {
        enabled: true,
        command: spec.command.to_string(),
        args: spec.args.iter().map(|a| a.to_string()).collect(),
        env: BTreeMap::new(),
        working_dir: ConnectorWorkingDir::Cwd,
        timeout_ms: spec.timeout_ms,
        tools: BTreeMap::from([(
            spec.tool.to_string(),
            ConnectorToolConfig {
                effect: ConnectorToolEffect::ReadLocal,
                approval: ConnectorApproval::AutoForReferenced,
                roots: vec!["cwd".to_string()],
                allowed_schemes: vec!["file".to_string()],
                max_output_bytes: spec.max_output_bytes,
            },
        )]),
    }
}

fn load_for_write(path: &Path) -> Result<Config> {
    if !path.exists() {
        bail!(
            "no hay configuración en {}. El modo LLM aún no está configurado: crea primero el fichero con model + providers y reintenta.",
            path.display()
        );
    }
    Config::load_from(path)
}

/// Instala (o activa) un plugin escribiendo el `config.toml` por el usuario.
/// `check` controla la comprobación del runtime (los tests la saltan).
pub fn install_at(path: &Path, name: &str, check: bool) -> Result<String> {
    let Some(spec) = find(name) else {
        let names: Vec<&str> = catalog().iter().map(|s| s.name).collect();
        bail!(
            "no existe el plugin {name:?}. Disponibles: {} (ver /plugins list)",
            names.join(", ")
        );
    };
    if check {
        check_runtime(spec)?;
    }
    let mut cfg = load_for_write(path)?;
    let msg = match cfg.connectors.get_mut(spec.connector) {
        None => {
            cfg.connectors
                .insert(spec.connector.to_string(), build_connector(spec));
            format!(
                "plugin '{}' instalado ({}). Ya puedes usarlo.",
                spec.name, spec.description
            )
        }
        Some(connector) if connector.enabled => {
            format!("el plugin '{}' ya estaba instalado.", spec.name)
        }
        Some(connector) => {
            connector.enabled = true;
            format!(
                "plugin '{}' activado (se conserva tu configuración personalizada).",
                spec.name
            )
        }
    };
    Config::save_to(&cfg, path)?;
    Ok(msg)
}

/// Desactiva un plugin (`enabled = false`, se conserva el bloque).
pub fn remove_at(path: &Path, name: &str) -> Result<String> {
    let Some(spec) = find(name) else {
        let names: Vec<&str> = catalog().iter().map(|s| s.name).collect();
        bail!(
            "no existe el plugin {name:?}. Disponibles: {} (ver /plugins list)",
            names.join(", ")
        );
    };
    let mut cfg = load_for_write(path)?;
    match cfg.connectors.get_mut(spec.connector) {
        Some(connector) if connector.enabled => {
            connector.enabled = false;
            Config::save_to(&cfg, path)?;
            Ok(format!("plugin '{}' desactivado.", spec.name))
        }
        _ => Ok(format!("el plugin '{}' no estaba instalado.", spec.name)),
    }
}

/// Atajos sobre la ruta real de configuración.
pub fn install(name: &str) -> Result<String> {
    install_at(&config_path(), name, true)
}

pub fn remove(name: &str) -> Result<String> {
    remove_at(&config_path(), name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Provider, SecurityConfig};
    use tempfile::TempDir;

    fn minimal_config() -> Config {
        Config {
            model: "zai/glm-4.6".into(),
            providers: BTreeMap::from([(
                "zai".to_string(),
                Provider {
                    base_url: "http://x".into(),
                    api: "anthropic".into(),
                    api_key: None,
                    api_key_env: None,
                    models: vec!["glm-4.6".into()],
                },
            )]),
            completion: "fuzzy".into(),
            security: SecurityConfig::default(),
            interpret_output: crate::config::InterpretOutputMode::Hint,
            connectors: BTreeMap::new(),
        }
    }

    fn config_file() -> (TempDir, std::path::PathBuf) {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        Config::save_to(&minimal_config(), &path).unwrap();
        (tmp, path)
    }

    #[test]
    fn catalogo_cubre_pdf_docx_xlsx() {
        assert_eq!(find_for_extension("pdf").unwrap().name, "documentos");
        assert_eq!(find_for_extension("docx").unwrap().name, "documentos");
        assert_eq!(find_for_extension("xlsx").unwrap().name, "documentos");
        assert_eq!(find_for_extension("PDF").unwrap().name, "documentos");
        assert!(find_for_extension("log").is_none());
        assert!(find("documentos").is_some());
        assert!(find("noexiste").is_none());
    }

    #[test]
    fn install_escribe_conector_habilitado() {
        let (_tmp, path) = config_file();
        let msg = install_at(&path, "documentos", false).unwrap();
        assert!(msg.contains("instalado"), "{msg}");

        let cfg = Config::load_from(&path).unwrap();
        let connector = cfg.connectors.get("markitdown").unwrap();
        assert!(connector.enabled);
        assert_eq!(connector.command, "uvx");
        assert_eq!(connector.args, vec!["markitdown-mcp==0.0.1a4"]);
        assert!(connector.tools.contains_key("convert_to_markdown"));
        assert_eq!(state(&cfg, &DOCUMENTOS), PluginState::Enabled);
    }

    #[test]
    fn install_idempotente_y_remove_desactiva() {
        let (_tmp, path) = config_file();
        install_at(&path, "documentos", false).unwrap();
        let again = install_at(&path, "documentos", false).unwrap();
        assert!(again.contains("ya estaba instalado"), "{again}");

        let msg = remove_at(&path, "documentos").unwrap();
        assert!(msg.contains("desactivado"), "{msg}");
        let cfg = Config::load_from(&path).unwrap();
        assert_eq!(state(&cfg, &DOCUMENTOS), PluginState::Disabled);

        // Reinstalar conserva el bloque y lo reactiva.
        let msg = install_at(&path, "documentos", false).unwrap();
        assert!(msg.contains("activado"), "{msg}");
        let cfg = Config::load_from(&path).unwrap();
        assert_eq!(state(&cfg, &DOCUMENTOS), PluginState::Enabled);
    }

    #[test]
    fn remove_sin_instalar_avisa() {
        let (_tmp, path) = config_file();
        let msg = remove_at(&path, "documentos").unwrap();
        assert!(msg.contains("no estaba instalado"), "{msg}");
    }

    #[test]
    fn nombre_desconocido_lista_disponibles() {
        let (_tmp, path) = config_file();
        let err = install_at(&path, "magia", false).unwrap_err();
        assert!(format!("{err}").contains("documentos"));
        let err = remove_at(&path, "magia").unwrap_err();
        assert!(format!("{err}").contains("documentos"));
    }

    #[test]
    fn sin_config_da_mensaje_accionable() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("config.toml");
        let err = install_at(&missing, "documentos", false).unwrap_err();
        assert!(format!("{err}").contains("model + providers"));
    }
}
