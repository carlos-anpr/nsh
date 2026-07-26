use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use super::{Effect, PlanOutcome, PlannedCommand, Planner, ShellContext};

pub struct AnthropicClient {
    agent: ureq::Agent,
    base_url: String,
    api_key: String,
    model: String,
}

impl AnthropicClient {
    pub fn new(base_url: &str, api_key: &str, model: &str) -> AnthropicClient {
        // TRAMPA 3: sin esto, un 401 llega como Err(StatusCode(401)) y perdemos
        // el cuerpo, que es donde viene "token expired or incorrect".
        // DESVIACION del plan: ureq 3.3.0 requiere .build() entre el builder y
        // new_agent() (new_agent es metodo de Config, no de ConfigBuilder).
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .new_agent();
        AnthropicClient {
            agent,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
        }
    }

    fn post(&self, body: Value) -> Result<Value> {
        let url = format!("{}/messages", self.base_url);
        // El cuerpo puede leerse solo una vez; clonamos por si hay reintento.
        let mut last_err: Option<anyhow::Error> = None;
        for _intent in 0..3u8 {
            let send_result = self.post_once(&url, &body);
            match send_result {
                Ok(v) => return Ok(v),
                Err(e) => {
                    let s = format!("{e:#}");
                    let transitorio = s.contains("close_notify")
                        || s.contains("unexpected eof")
                        || s.contains("connection reset")
                        || s.contains("broken pipe")
                        || s.contains("timed out");
                    if transitorio {
                        last_err = Some(e);
                        std::thread::sleep(std::time::Duration::from_millis(300));
                        continue;
                    }
                    // No transitorio (4xx/5xx con cuerpo, error de parseo, etc.): devolver ya.
                    return Err(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("reintentos agotados sin error concreto")))
    }

    fn post_once(&self, url: &str, body: &Value) -> Result<Value> {
        let mut resp = self
            .agent
            .post(url)
            .header("x-api-key", &self.api_key) // TRAMPA 2: .header, no .set
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .send_json(body)?;

        let status = resp.status().as_u16();
        // TRAMPA 2: en ureq 3.x el cuerpo se lee con body_mut().read_json().
        let parsed: Value = resp.body_mut().read_json()?;

        if status != 200 {
            let msg = parsed["error"]["message"]
                .as_str()
                .unwrap_or("error sin mensaje");
            bail!("el proveedor devolvio {status}: {msg}");
        }
        Ok(parsed)
    }

    fn system_prompt(ctx: &ShellContext) -> String {
        let mut s = format!(
            "Eres el planificador de nsh, una shell asistida.\n\
             Traduces la peticion del usuario a UN solo comando de bash para Linux.\n\
             Directorio actual: {}\n\
             Sistema: {}\n\
             Reglas:\n\
             - Un unico comando. Puedes encadenar con && o | si hace falta.\n\
             - Nada de comandos interactivos que se queden esperando (vim, top, ssh).\n\
             - expected_effect: ReadOnly si no modifica nada, Modifies si crea o cambia\n\
               ficheros, Destructive si borra, sobreescribe o es irreversible.\n\
             - Ante la duda entre dos niveles, elige el mas peligroso.\n",
            ctx.cwd.display(),
            ctx.os
        );

        if !ctx.entries.is_empty() {
            s.push_str("\nFicheros y directorios en el directorio actual (nombres EXACTOS):\n");
            for entry in &ctx.entries {
                s.push_str(&format!("  {entry}\n"));
            }
            s.push_str(
                "\nReglas sobre nombres de fichero:\n\
                 - Usa los nombres EXACTAMENTE como aparecen arriba. NUNCA cambies mayusculas ni\n\
                   minusculas: en Linux README.md y Readme.md son ficheros DISTINTOS.\n\
                 - Si el usuario menciona un fichero que NO esta en la lista, no te lo inventes:\n\
                   propon un comando que lo busque (ls, find), no uno que lo asuma.\n",
            );
        }

        if !ctx.referenced.is_empty() {
            s.push_str("\nFicheros que el usuario referencio con @ (rutas YA VERIFICADAS por nsh, existen):\n");
            for ref_path in &ctx.referenced {
                s.push_str(&format!("  {ref_path}\n"));
            }
            s.push_str("Usa estas rutas tal cual.\n");
        }

        if !ctx.recent.is_empty() {
            s.push_str("\nComandos recientes (comando -> codigo de salida):\n");
            for (cmd, code) in &ctx.recent {
                s.push_str(&format!("  {cmd} -> {code}\n"));
            }
        }
        if let Some(out) = &ctx.last_output {
            s.push_str(&format!("\nSalida del ultimo comando:\n{out}\n"));
        }
        s
    }

    fn explain_system_prompt(ctx: &ShellContext) -> String {
        let mut s = format!(
            "Eres el asistente de nsh. Respondes en texto a la pregunta del usuario sobre documentos o salidas ya disponibles.\n\
             No traduzcas la peticion a comandos ni hables de herramientas.\n\
             Directorio actual: {}\n\
             Sistema: {}\n",
            ctx.cwd.display(),
            ctx.os
        );

        if !ctx.referenced.is_empty() {
            s.push_str("\nReferencias verificadas:\n");
            for ref_path in &ctx.referenced {
                s.push_str(&format!("  {ref_path}\n"));
            }
        }

        if let Some(out) = &ctx.last_output {
            s.push_str(&format!("\nSalida o contenido disponible:\n{out}\n"));
        }

        s.push_str(
            "\nSi el usuario pide un resumen o explicacion del documento adjunto, responde directamente con ese resumen o explicacion.\n",
        );
        s
    }

    fn tool_schema() -> Value {
        json!({
            "name": "propose_command",
            "description": "Propone un comando bash para la peticion del usuario",
            "input_schema": {
                "type": "object",
                "properties": {
                    "explanation": {"type":"string","description":"Que hace el comando, una frase"},
                    "command":     {"type":"string","description":"El comando bash exacto"},
                    "expected_effect": {
                        "type":"string",
                        "enum":["ReadOnly","Modifies","Destructive"]
                    }
                },
                "required": ["explanation","command","expected_effect"]
            }
        })
    }

    pub fn build_plan_body(&self, request: &str, ctx: &ShellContext) -> Value {
        json!({
            "model": self.model,
            "max_tokens": 1024,
            "system": Self::system_prompt(ctx),
            "tools": [Self::tool_schema()],
            "tool_choice": {"type":"tool","name":"propose_command"},
            "messages": [{"role":"user","content": request}]
        })
    }
}

/// Parsea la respuesta de un plan. Extraido a funcion libre para poder testearlo
/// SIN red (tests L1, L2, L3 de la fase 2).
///
/// TRAMPA 1: el array `content` puede traer un bloque "text" ANTES del
/// "tool_use". Hay que BUSCARLO, jamas indexar content[0].
pub fn parse_plan_response(resp: &Value) -> Result<PlanOutcome> {
    let content = resp["content"]
        .as_array()
        .ok_or_else(|| anyhow!("respuesta sin array `content`"))?;
    let tool = content.iter().find(|b| b["type"] == "tool_use");

    if let Some(tool) = tool {
        let input = &tool["input"];
        return Ok(PlanOutcome::Command(PlannedCommand {
            explanation: input["explanation"].as_str().unwrap_or("").to_string(),
            command: input["command"]
                .as_str()
                .ok_or_else(|| anyhow!("la herramienta no devolvio `command`"))?
                .to_string(),
            expected_effect: Effect::parse(input["expected_effect"].as_str().unwrap_or("")),
        }));
    }

    let text = content
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if !text.trim().is_empty() {
        return Ok(PlanOutcome::DirectText(text));
    }

    Err(anyhow!(
        "el modelo no devolvio ni herramienta ni texto; stop_reason={}",
        resp["stop_reason"]
    ))
}

/// Concatena los bloques de texto de una respuesta (para /why).
pub fn parse_explain_response(resp: &Value) -> Result<String> {
    let content = resp["content"]
        .as_array()
        .ok_or_else(|| anyhow!("sin content"))?;
    Ok(content
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n"))
}

impl Planner for AnthropicClient {
    fn plan(&self, request: &str, ctx: &ShellContext) -> Result<PlanOutcome> {
        let body = self.build_plan_body(request, ctx);

        let resp = self.post(body)?;
        parse_plan_response(&resp)
    }

    fn explain(&self, question: &str, ctx: &ShellContext) -> Result<String> {
        let body = json!({
            "model": self.model,
            "max_tokens": 1024,
            "system": Self::explain_system_prompt(ctx),
            "messages": [{"role":"user","content": question}]
        });
        let resp = self.post(body)?;
        parse_explain_response(&resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // L1 — TRAMPA 1: un bloque "text" ANTES del "tool_use". Un parser que
    // indexara content[0] fallaria aqui. Este test lo blinda.
    #[test]
    fn l1_text_antes_de_tool_use() {
        let resp = json!({
            "content": [
                {"type": "text", "text": "Voy a listar los archivos."},
                {"type": "tool_use", "id": "call_1", "name": "propose_command",
                 "input": {
                    "explanation": "Lista archivos",
                    "command": "ls -la",
                    "expected_effect": "ReadOnly"
                 }}
            ],
            "stop_reason": "tool_use"
        });
        let p = parse_plan_response(&resp).expect("debe parsear");
        let PlanOutcome::Command(p) = p else {
            panic!("esperaba comando");
        };
        assert_eq!(p.command, "ls -la");
        assert_eq!(p.expected_effect, Effect::ReadOnly);
        assert_eq!(p.explanation, "Lista archivos");
    }

    // L2/L31 — sin tool_use pero con texto: respuesta directa.
    #[test]
    fn l31_sin_tool_use_con_texto_es_respuesta() {
        let resp = json!({
            "content": [{"type": "text", "text": "No se usar la herramienta."}],
            "stop_reason": "end_turn"
        });
        let out = parse_plan_response(&resp).unwrap();
        let PlanOutcome::DirectText(text) = out else {
            panic!("esperaba texto directo");
        };
        assert!(text.contains("No se usar la herramienta"));
    }

    #[test]
    fn l32_sin_tool_use_sin_texto_es_error() {
        let resp = json!({
            "content": [],
            "stop_reason": "end_turn"
        });
        let err = parse_plan_response(&resp).unwrap_err();
        assert!(format!("{err}").contains("ni herramienta ni texto"));
    }

    // L3 — expected_effect con valor desconocido -> Destructive (lo mas restrictivo).
    #[test]
    fn l3_effect_desconocido_es_destructive() {
        let resp = json!({
            "content": [{
                "type": "tool_use", "name": "propose_command",
                "input": {"explanation": "x", "command": "echo hola", "expected_effect": "Nonsense"}
            }]
        });
        let p = parse_plan_response(&resp).unwrap();
        let PlanOutcome::Command(p) = p else {
            panic!("esperaba comando");
        };
        assert_eq!(p.expected_effect, Effect::Destructive);
    }

    // ---------- tests de RED (#[ignore], gastan cuota) ----------
    // Se corren a mano:
    //   cargo test -- --ignored llm
    // Necesitan ~/.config/nsh/config.toml con la key real (chmod 600).

    fn ctx() -> super::super::ShellContext {
        super::super::ShellContext {
            cwd: std::env::current_dir().unwrap(),
            os: "linux".into(),
            recent: vec![],
            last_output: None,
            entries: vec![],
            referenced: vec![],
        }
    }

    fn client_from_config() -> AnthropicClient {
        let cfg = crate::config::Config::load()
            .expect("necesitas ~/.config/nsh/config.toml (chmod 600) con la key real");
        let (_p, provider, mname) = cfg.resolve().expect("resolve");
        let key = provider.key().expect("key");
        AnthropicClient::new(&provider.base_url, &key, mname)
    }

    #[test]
    #[ignore]
    fn llm_plan_real_lectura() {
        let c = client_from_config();
        let planned = c
            .plan("lista los ficheros de este directorio", &ctx())
            .unwrap();
        let PlanOutcome::Command(planned) = planned else {
            panic!("esperaba comando");
        };
        eprintln!(
            "[lectura] command={} effect={:?}",
            planned.command, planned.expected_effect
        );
        let cmd = planned.command.to_lowercase();
        assert!(
            cmd.contains("ls") || cmd.contains("find") || cmd.contains("dir"),
            "esperaba un listado, vino: {}",
            planned.command
        );
        assert_eq!(planned.expected_effect, Effect::ReadOnly);
    }

    #[test]
    #[ignore]
    fn llm_plan_real_destructivo() {
        let c = client_from_config();
        let planned = c
            .plan("borra todos los ficheros .tmp recursivamente", &ctx())
            .unwrap();
        let PlanOutcome::Command(planned) = planned else {
            panic!("esperaba comando");
        };
        eprintln!(
            "[destructivo] command={} effect={:?}",
            planned.command, planned.expected_effect
        );
        assert_eq!(
            planned.expected_effect,
            Effect::Destructive,
            "un borrado recursivo debe ser Destructive (vino {:?} con {})",
            planned.expected_effect,
            planned.command
        );
    }

    #[test]
    #[ignore]
    fn llm_key_rota_da_401() {
        // TRAMPA 3: gracias a http_status_as_error(false) podemos leer el cuerpo
        // del 401 y mostrar "token expired or incorrect".
        let c = AnthropicClient::new(
            "https://api.z.ai/api/anthropic/v1",
            "KEY_FALSA_12345",
            "glm-5.2",
        );
        let err = c.plan("ls", &ctx()).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("401"), "esperaba un 401 legible, vino: {msg}");
    }
}
