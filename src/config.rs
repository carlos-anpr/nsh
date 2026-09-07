use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub base_url: String,
    /// Estilo de API compatible: "anthropic" u "openai".
    pub api: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub models: Vec<String>,
    /// Esfuerzo de razonamiento para APIs que lo soportan (p.ej. Ollama).
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    #[serde(default = "default_security_roots")]
    pub roots: Vec<String>,
    #[serde(default)]
    pub extra_roots: Vec<PathBuf>,
    #[serde(default = "default_redact_sensitive_output")]
    pub redact_sensitive_output: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum InterpretOutputMode {
    #[default]
    Hint,
    Auto,
    Never,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorWorkingDir {
    #[default]
    Cwd,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorToolEffect {
    ReadLocal,
    ReadExternal,
    WriteLocal,
    SideEffectExternal,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorApproval {
    #[default]
    AutoForReferenced,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorToolConfig {
    #[serde(default)]
    pub effect: ConnectorToolEffect,
    #[serde(default)]
    pub approval: ConnectorApproval,
    #[serde(default = "default_connector_tool_roots")]
    pub roots: Vec<String>,
    #[serde(default)]
    pub allowed_schemes: Vec<String>,
    #[serde(default = "default_connector_max_output_bytes")]
    pub max_output_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConnectorConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub working_dir: ConnectorWorkingDir,
    #[serde(default = "default_connector_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default)]
    pub tools: BTreeMap<String, ConnectorToolConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// "proveedor/modelo", p.ej. "zai/glm-5.2"
    pub model: String,
    pub providers: BTreeMap<String, Provider>,
    #[serde(default = "default_completion")]
    pub completion: String,
    #[serde(default)]
    pub security: SecurityConfig,
    #[serde(default)]
    pub interpret_output: InterpretOutputMode,
    #[serde(default)]
    pub connectors: BTreeMap<String, ConnectorConfig>,
}

fn default_completion() -> String {
    "fuzzy".to_string()
}

fn default_security_roots() -> Vec<String> {
    vec!["cwd".to_string()]
}

fn default_redact_sensitive_output() -> bool {
    true
}

fn default_connector_tool_roots() -> Vec<String> {
    vec!["cwd".to_string()]
}

fn default_connector_max_output_bytes() -> usize {
    1024 * 1024
}

fn default_connector_timeout_ms() -> u64 {
    30_000
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            roots: default_security_roots(),
            extra_roots: Vec::new(),
            redact_sensitive_output: default_redact_sensitive_output(),
        }
    }
}

pub fn config_path() -> PathBuf {
    // Override solo para tests: apuntar a un config inexistente y simular
    // "modo LLM no disponible" (caso L9) sin tocar el config real del usuario.
    if let Ok(p) = std::env::var("NSH_CONFIG_PATH") {
        return PathBuf::from(p);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("nsh")
        .join("config.toml")
}

impl Config {
    pub fn load() -> Result<Config> {
        Self::load_from(&config_path())
    }

    /// Costura para tests: igual que `load` pero desde una ruta dada.
    pub fn load_from(path: &Path) -> Result<Config> {
        if !path.exists() {
            bail!(
                "no hay configuracion en {}. Crea el fichero (chmod 600) para usar el modo LLM.",
                path.display()
            );
        }
        check_permissions(path)?;
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("no se pudo leer {}", path.display()))?;
        let mut cfg: Config = toml::from_str(&text)
            .with_context(|| format!("{} no es un TOML valido", path.display()))?;
        cfg.normalize_security()?;
        cfg.normalize_connectors()?;
        cfg.resolve()?; // falla pronto si el modelo activo no existe
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path();
        Self::save_to(self, &path)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        set_600(path)?;
        Ok(())
    }

    /// Separa "proveedor/modelo" y comprueba que ambos existen.
    pub fn resolve(&self) -> Result<(&str, &Provider, &str)> {
        let (pname, mname) = self.model.split_once('/').with_context(|| {
            format!("`model` debe ser \"proveedor/modelo\", es {:?}", self.model)
        })?;
        let provider = self
            .providers
            .get(pname)
            .with_context(|| format!("el proveedor {:?} no esta en [providers]", pname))?;
        if !provider.models.is_empty() && !provider.models.iter().any(|m| m == mname) {
            bail!(
                "el modelo {:?} no esta listado en el proveedor {:?} (tiene: {})",
                mname,
                pname,
                provider.models.join(", ")
            );
        }
        Ok((pname, provider, mname))
    }

    fn normalize_security(&mut self) -> Result<()> {
        if self.security.roots.is_empty() {
            self.security.roots = default_security_roots();
        }
        for root in &self.security.roots {
            if root != "cwd" {
                bail!(
                    "security.roots solo soporta \"cwd\" en esta fase (vino {:?})",
                    root
                );
            }
        }

        let mut canonical = Vec::new();
        for root in &self.security.extra_roots {
            if !root.is_absolute() {
                bail!(
                    "security.extra_roots debe contener rutas absolutas: {}",
                    root.display()
                );
            }
            if !root.exists() {
                bail!(
                    "security.extra_roots contiene una ruta que no existe: {}",
                    root.display()
                );
            }
            let resolved = root
                .canonicalize()
                .with_context(|| format!("no se pudo canonicalizar {}", root.display()))?;
            if !canonical.iter().any(|seen| seen == &resolved) {
                canonical.push(resolved);
            }
        }
        self.security.extra_roots = canonical;
        Ok(())
    }

    fn normalize_connectors(&mut self) -> Result<()> {
        for (name, connector) in &mut self.connectors {
            if connector.timeout_ms == 0 {
                bail!("connectors.{name}.timeout_ms debe ser mayor que 0");
            }
            if connector.enabled && connector.command.trim().is_empty() {
                bail!("connectors.{name}.command no puede estar vacio si enabled = true");
            }
            for (tool_name, tool) in &mut connector.tools {
                if tool.roots.is_empty() {
                    tool.roots = default_connector_tool_roots();
                }
                if tool.max_output_bytes == 0 {
                    bail!(
                        "connectors.{name}.tools.{tool_name}.max_output_bytes debe ser mayor que 0"
                    );
                }
                for root in &tool.roots {
                    if root == "cwd" {
                        continue;
                    }
                    let path = Path::new(root);
                    if !path.is_absolute() {
                        bail!(
                            "connectors.{name}.tools.{tool_name}.roots solo admite \"cwd\" o rutas absolutas: {root}"
                        );
                    }
                    if !path.exists() {
                        bail!(
                            "connectors.{name}.tools.{tool_name}.roots contiene una ruta que no existe: {}",
                            path.display()
                        );
                    }
                }
                tool.allowed_schemes = tool
                    .allowed_schemes
                    .iter()
                    .map(|scheme| scheme.to_ascii_lowercase())
                    .collect();
            }
        }
        Ok(())
    }
}

impl Provider {
    pub fn key(&self) -> Result<String> {
        if let Some(var) = &self.api_key_env {
            return std::env::var(var)
                .with_context(|| format!("la variable de entorno {var} no esta puesta"));
        }
        if let Some(k) = &self.api_key {
            return Ok(k.clone());
        }
        bail!("el proveedor no tiene ni api_key ni api_key_env")
    }
}

/// Con `api_key` en linea, un fichero legible por otros es un fallo, no un aviso.
fn check_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)?.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        bail!(
            "{} tiene permisos {:o}: puede contener credenciales. Arreglalo con:\n    chmod 600 {}",
            path.display(),
            mode,
            path.display()
        );
    }
    Ok(())
}

fn set_600(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn write_tmp(text: &str, mode: u32) -> (tempfile::NamedTempFile, PathBuf) {
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(f.path(), text).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(f.path(), std::fs::Permissions::from_mode(mode)).unwrap();
        let path = f.path().to_path_buf();
        (f, path)
    }

    const OK_TOML: &str = r#"
model = "zai/glm-5.2"

[providers.zai]
base_url = "https://api.z.ai/api/anthropic/v1"
api = "anthropic"
api_key = "una-key"
models = ["glm-5.2", "glm-4.7"]
"#;

    #[test]
    fn resolve_bien() {
        let (_g, path) = write_tmp(OK_TOML, 0o600);
        let cfg = Config::load_from(&path).unwrap();
        let (pname, _prov, mname) = cfg.resolve().unwrap();
        assert_eq!(pname, "zai");
        assert_eq!(mname, "glm-5.2");
    }

    #[test]
    fn permisos_644_falla() {
        let (_g, path) = write_tmp(OK_TOML, 0o644);
        let err = Config::load_from(&path).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("chmod 600"),
            "mensaje sin ayuda del chmod: {msg}"
        );
    }

    #[test]
    fn model_inexistente_falla() {
        // proveedor existe pero modelo no listado
        let bad2 = r#"
model = "zai/noexiste"

[providers.zai]
base_url = "http://x"
api = "anthropic"
api_key = "k"
models = ["glm-5.2"]
"#;
        let (_g, path) = write_tmp(bad2, 0o600);
        let err = Config::load_from(&path).unwrap_err();
        assert!(format!("{err}").contains("no esta listado"));

        // proveedor que no existe
        let bad1 = r#"
model = "noexiste/x"

[providers.otro]
base_url = "http://x"
api = "anthropic"
api_key = "k"
models = ["x"]
"#;
        let (_g, path) = write_tmp(bad1, 0o600);
        let err = Config::load_from(&path).unwrap_err();
        assert!(format!("{err}").contains("no esta en"));
    }

    #[test]
    fn security_extra_root_relativa_falla() {
        let bad = r#"
model = "zai/glm-5.2"

[providers.zai]
base_url = "http://x"
api = "anthropic"
api_key = "k"
models = ["glm-5.2"]

[security]
extra_roots = ["relativa"]
"#;
        let (_g, path) = write_tmp(bad, 0o600);
        let err = Config::load_from(&path).unwrap_err();
        assert!(format!("{err}").contains("rutas absolutas"));
    }

    #[test]
    fn security_extra_root_se_canonicaliza() {
        let tmp = tempfile::TempDir::new().unwrap();
        let real = tmp.path().join("real-root");
        std::fs::create_dir(&real).unwrap();
        let link = tmp.path().join("root-link");
        symlink(&real, &link).unwrap();

        let text = format!(
            r#"
model = "zai/glm-5.2"

[providers.zai]
base_url = "http://x"
api = "anthropic"
api_key = "k"
models = ["glm-5.2"]

[security]
extra_roots = ["{}"]
"#,
            link.display()
        );
        let (_g, path) = write_tmp(&text, 0o600);
        let cfg = Config::load_from(&path).unwrap();
        assert_eq!(cfg.security.extra_roots, vec![real.canonicalize().unwrap()]);
    }

    #[test]
    fn redact_sensitive_output_default_true() {
        let (_g, path) = write_tmp(OK_TOML, 0o600);
        let cfg = Config::load_from(&path).unwrap();
        assert!(cfg.security.redact_sensitive_output);
    }

    #[test]
    fn connector_root_relativo_falla() {
        let bad = r#"
model = "zai/glm-5.2"

[providers.zai]
base_url = "http://x"
api = "anthropic"
api_key = "k"
models = ["glm-5.2"]

[connectors.markitdown]
enabled = true
command = "uvx"

[connectors.markitdown.tools.convert_to_markdown]
roots = ["relativo"]
"#;
        let (_g, path) = write_tmp(bad, 0o600);
        let err = Config::load_from(&path).unwrap_err();
        assert!(format!("{err}").contains("solo admite \"cwd\" o rutas absolutas"));
    }
}
