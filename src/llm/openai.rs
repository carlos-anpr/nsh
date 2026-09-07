use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use super::{Effect, PlanOutcome, PlannedCommand, Planner, ShellContext};

/// Cliente para APIs compatibles con OpenAI Chat Completions.
///
/// Ollama expone este contrato en `/v1/chat/completions` y autentica las
/// peticiones cloud con `Authorization: Bearer ...`.
pub struct OpenAiClient {
    agent: ureq::Agent,
    base_url: String,
    api_key: String,
    model: String,
    reasoning_effort: Option<String>,
}

impl OpenAiClient {
    pub fn new(
        base_url: &str,
        api_key: &str,
        model: &str,
        reasoning_effort: Option<&str>,
    ) -> OpenAiClient {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            // Misma proteccion que el cliente Anthropic: sin limite, una
            // llamada sin respuesta colgaria el REPL para siempre.
            .timeout_global(Some(std::time::Duration::from_secs(60)))
            .build()
            .new_agent();
        OpenAiClient {
            agent,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
            reasoning_effort: reasoning_effort.map(str::to_string),
        }
    }

    fn post(&self, body: Value) -> Result<Value> {
        let url = format!("{}/chat/completions", self.base_url);
        let mut last_err: Option<anyhow::Error> = None;
        for _intent in 0..3u8 {
            match self.post_once(&url, &body) {
                Ok(value) => return Ok(value),
                Err(error) => {
                    let message = format!("{error:#}");
                    let transient = message.contains("close_notify")
                        || message.contains("unexpected eof")
                        || message.contains("connection reset")
                        || message.contains("broken pipe")
                        || message.contains("timed out");
                    if !transient {
                        return Err(error);
                    }
                    last_err = Some(error);
                    std::thread::sleep(std::time::Duration::from_millis(300));
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("reintentos agotados sin error concreto")))
    }

    fn post_once(&self, url: &str, body: &Value) -> Result<Value> {
        let authorization = format!("Bearer {}", self.api_key);
        let mut response = self
            .agent
            .post(url)
            .header("Authorization", &authorization)
            .header("content-type", "application/json")
            .send_json(body)?;

        let status = response.status().as_u16();
        let parsed: Value = response.body_mut().read_json()?;
        if status != 200 {
            let message = parsed["error"]["message"]
                .as_str()
                .unwrap_or("error sin mensaje");
            bail!("el proveedor devolvio {status}: {message}");
        }
        Ok(parsed)
    }

    fn system_prompt(ctx: &ShellContext) -> String {
        let mut prompt = format!(
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
            prompt.push_str("\nFicheros y directorios en el directorio actual (nombres EXACTOS):\n");
            for entry in &ctx.entries {
                prompt.push_str(&format!("  {entry}\n"));
            }
            prompt.push_str(
                "\nReglas sobre nombres de fichero:\n\
                 - Usa los nombres EXACTAMENTE como aparecen arriba. NUNCA cambies mayusculas ni\n\
                   minusculas: en Linux README.md y Readme.md son ficheros DISTINTOS.\n\
                 - Si el usuario menciona un fichero que NO esta en la lista, no te lo inventes:\n\
                   propon un comando que lo busque (ls, find), no uno que lo asuma.\n",
            );
        }

        if !ctx.referenced.is_empty() {
            prompt.push_str(
                "\nFicheros que el usuario referencio con @ (rutas YA VERIFICADAS por nsh, existen):\n",
            );
            for referenced in &ctx.referenced {
                prompt.push_str(&format!("  {referenced}\n"));
            }
            prompt.push_str("Usa estas rutas tal cual.\n");
        }

        if !ctx.recent.is_empty() {
            prompt.push_str("\nComandos recientes (comando -> codigo de salida):\n");
            for (command, code) in &ctx.recent {
                prompt.push_str(&format!("  {command} -> {code}\n"));
            }
        }
        if let Some(output) = &ctx.last_output {
            prompt.push_str(&format!("\nSalida del ultimo comando:\n{output}\n"));
        }
        prompt
    }

    fn explain_system_prompt(ctx: &ShellContext) -> String {
        let mut prompt = format!(
            "Eres el asistente de nsh. Respondes en texto a la pregunta del usuario sobre documentos o salidas ya disponibles.\n\
             No traduzcas la peticion a comandos ni hables de herramientas.\n\
             Directorio actual: {}\n\
             Sistema: {}\n",
            ctx.cwd.display(),
            ctx.os
        );

        if !ctx.referenced.is_empty() {
            prompt.push_str("\nReferencias verificadas:\n");
            for referenced in &ctx.referenced {
                prompt.push_str(&format!("  {referenced}\n"));
            }
        }
        if let Some(output) = &ctx.last_output {
            prompt.push_str(&format!("\nSalida o contenido disponible:\n{output}\n"));
        }
        prompt.push_str(
            "\nSi el usuario pide un resumen o explicacion del documento adjunto, responde directamente con ese resumen o explicacion.\n",
        );
        prompt
    }

    fn tool_parameters() -> Value {
        json!({
            "type": "object",
            "properties": {
                "explanation": {"type":"string","description":"Que hace el comando, una frase"},
                "command": {"type":"string","description":"El comando bash exacto"},
                "expected_effect": {
                    "type":"string",
                    "enum":["ReadOnly","Modifies","Destructive"]
                }
            },
            "required": ["explanation", "command", "expected_effect"]
        })
    }

    pub fn build_plan_body(&self, request: &str, ctx: &ShellContext) -> Value {
        let mut body = json!({
            "model": self.model,
            "max_tokens": 1024,
            "messages": [
                {"role":"system","content": Self::system_prompt(ctx)},
                {"role":"user","content": request}
            ],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "propose_command",
                    "description": "Propone un comando bash para la peticion del usuario",
                    "parameters": Self::tool_parameters()
                }
            }],
            "tool_choice": {
                "type": "function",
                "function": {"name": "propose_command"}
            }
        });
        if let Some(effort) = &self.reasoning_effort {
            body["reasoning_effort"] = json!(effort);
        }
        body
    }

    fn build_explain_body(&self, question: &str, ctx: &ShellContext) -> Value {
        let mut body = json!({
            "model": self.model,
            "max_tokens": 1024,
            "messages": [
                {"role":"system","content": Self::explain_system_prompt(ctx)},
                {"role":"user","content": question}
            ]
        });
        if let Some(effort) = &self.reasoning_effort {
            body["reasoning_effort"] = json!(effort);
        }
        body
    }
}

fn first_message(resp: &Value) -> Result<&Value> {
    resp["choices"]
        .as_array()
        .and_then(|choices| choices.first())
        .map(|choice| &choice["message"])
        .ok_or_else(|| anyhow!("respuesta sin choices/message"))
}

fn tool_input(call: &Value) -> Result<Value> {
    let arguments = &call["function"]["arguments"];
    match arguments {
        Value::String(raw) => serde_json::from_str(raw)
            .with_context(|| "la herramienta devolvio argumentos JSON invalidos"),
        Value::Object(_) => Ok(arguments.clone()),
        _ => bail!("la herramienta no devolvio argumentos"),
    }
}

pub fn parse_plan_response(resp: &Value) -> Result<PlanOutcome> {
    let message = first_message(resp)?;
    if let Some(call) = message["tool_calls"]
        .as_array()
        .and_then(|calls| calls.iter().find(|call| {
            call["function"]["name"] == "propose_command"
        }))
    {
        let input = tool_input(call)?;
        return Ok(PlanOutcome::Command(PlannedCommand {
            explanation: input["explanation"].as_str().unwrap_or("").to_string(),
            command: input["command"]
                .as_str()
                .ok_or_else(|| anyhow!("la herramienta no devolvio `command`"))?
                .to_string(),
            expected_effect: Effect::parse(input["expected_effect"].as_str().unwrap_or("")),
        }));
    }

    let text = message["content"].as_str().unwrap_or("");
    if !text.trim().is_empty() {
        return Ok(PlanOutcome::DirectText(text.to_string()));
    }
    Err(anyhow!(
        "el modelo no devolvio ni herramienta ni texto; finish_reason={}",
        resp["choices"][0]["finish_reason"]
    ))
}

pub fn parse_explain_response(resp: &Value) -> Result<String> {
    let text = first_message(resp)?["content"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string();
    if text.is_empty() {
        bail!("el modelo no devolvio texto para la explicacion");
    }
    Ok(text)
}

impl Planner for OpenAiClient {
    fn plan(&self, request: &str, ctx: &ShellContext) -> Result<PlanOutcome> {
        parse_plan_response(&self.post(self.build_plan_body(request, ctx))?)
    }

    fn explain(&self, question: &str, ctx: &ShellContext) -> Result<String> {
        parse_explain_response(&self.post(self.build_explain_body(question, ctx))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parsea_tool_call_openai() {
        let response = json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "content": "",
                    "tool_calls": [{
                        "type": "function",
                        "function": {
                            "name": "propose_command",
                            "arguments": "{\"explanation\":\"Lista\",\"command\":\"ls\",\"expected_effect\":\"ReadOnly\"}"
                        }
                    }]
                }
            }]
        });
        let PlanOutcome::Command(command) = parse_plan_response(&response).unwrap() else {
            panic!("esperaba un comando");
        };
        assert_eq!(command.command, "ls");
        assert_eq!(command.expected_effect, Effect::ReadOnly);
    }

    #[test]
    fn texto_directo_openai() {
        let response = json!({
            "choices": [{"finish_reason": "stop", "message": {"content": "respuesta"}}]
        });
        assert!(matches!(
            parse_plan_response(&response).unwrap(),
            PlanOutcome::DirectText(text) if text == "respuesta"
        ));
    }
}
