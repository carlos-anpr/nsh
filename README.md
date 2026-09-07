<div align="center">

# nsh

### *Shell interactiva asistida por modelos de lenguaje*

[![Rust](https://img.shields.io/badge/Rust-2024-dea584?style=flat-square&logo=rust&logoColor=black)](https://www.rust-lang.org)
[![MCP](https://img.shields.io/badge/MCP-Model_Context_Protocol-7c3aed?style=flat-square)](https://modelcontextprotocol.io)
[![LLM](https://img.shields.io/badge/LLM-Anthropic_%7C_OpenAI_%7C_Ollama-ff6b6b?style=flat-square)](https://ollama.com)
[![License](https://img.shields.io/badge/License-MIT-339933?style=flat-square)](LICENSE)

**Una shell en Rust que interpreta peticiones en lenguaje natural, planifica el comando exacto y somete su ejecución a aprobación del usuario.**

</div>

---

## Descripción

nsh envuelve una sesión de Bash real y persistente — `cd`, `alias` y `export` sobreviven entre comandos — e incorpora un planificador LLM como capa adicional de interacción:

```text
nsh ~/Proyectos/app ❯ !ls -la logs/
nsh ~/Proyectos/app ❯ busca los errores 500 de hoy y dime qué IP los concentra
nsh ~/Proyectos/app ❯ resumen de @informe.pdf
nsh ~/Proyectos/app ❯ /why
```

- **`!comando`** — Ejecución directa en Bash, sin intervención del LLM ni de la política.
- **Texto natural** — El LLM planifica el comando; el usuario lo aprueba antes de ejecutarlo.
- **`@fichero`** — Referencia a ficheros reales, verificados antes de la llamada al modelo, con autocompletado por Tab.
- **`/why`** — Interpretación de la última salida por parte del modelo.

![Demo de nsh](docs/demo/nsh-demo.gif)

## Instalación (Linux)

Requisitos: **Rust 1.85+** (edición 2024), Bash y, para el modo LLM, una clave de API.

```bash
git clone https://github.com/cursospotiapp/nsh.git
cd nsh && cargo build --release
cargo install --path .
```

La configuración reside en `~/.config/nsh/config.toml` (permisos `600`, obligatorios; el programa rechaza arrancar con el fichero legible por otros):

```toml
model = "zai/glm-4.7"

[providers.zai]
base_url = "https://api.z.ai/api/anthropic/v1"
api = "anthropic"
api_key = "TU_API_KEY"
models = ["glm-4.7"]
```

<details>
<summary><b>Otros proveedores</b></summary>

```toml
# Anthropic
[providers.anthropic]
base_url = "https://api.anthropic.com"
api = "anthropic"
api_key = "TU_API_KEY"

# OpenAI (o LM Studio, vLLM… cualquier API compatible)
[providers.openai]
base_url = "https://api.openai.com/v1"
api = "openai"
api_key = "TU_API_KEY"

# Ollama — ejecución íntegramente local
[providers.ollama]
base_url = "http://localhost:11434/v1"
api = "openai"
api_key = "ollama"
models = ["qwen3:8b"]
```

Para no almacenar la clave en el fichero: `api_key_env = "MI_VARIABLE"`.
</details>

```bash
nsh
```

Sin configuración, nsh funciona como shell convencional: los comandos `!` no requieren LLM.

## Superficie de interacción

| Entrada | Ejemplo | Comportamiento |
|---|---|---|
| `!comando` | `!find . -name "*.log"` | Ejecución directa en la sesión persistente |
| Texto natural | `qué fichero pesa más de 100 MB` | El LLM planifica el comando; el usuario elige `[e]jecutar`, `[c]ancelar` o `[m]odificar` |
| `@fichero` | `resumen de @contrato.pdf` | Verificación de ruta y envío al modelo (con el plugin `documentos`: pdf/docx/xlsx vía MCP) |
| `/why` | `/why` | El modelo interpreta la última salida y propone el siguiente paso |
| `/fix` | `/fix` | Reintento del último comando fallido, con el error en el contexto |
| `/yolo` | `/yolo` | Alterna el modo de aprobación automática |
| `/models` | `/models` | Cambio de modelo en caliente |
| `/plugins` | `/plugins install documentos` | Instalación de capacidades opcionales sin reiniciar |

## Política de ejecución

Todo comando planificado por el LLM atraviesa una política de clasificación implementada en Rust — lexer de Bash propio, ajena al prompt — que decide antes de solicitar aprobación:

| Situación | `confirm` (defecto) | `yolo` |
|---|---|---|
| Lecturas y búsquedas | ejecuta | ejecuta |
| Escrituras y borrados ordinarios | confirma | ejecuta |
| `chmod 777`, mutaciones del sistema, borrado masivo | confirma | **confirma** |
| Zonas de sistema, escalada de privilegios, `curl … \| sh` | **deniega** | **deniega** |

Los comandos `!` del usuario no pasan por la política. Las salidas que alcanzan rutas sensibles (`~/.ssh`, credenciales) se omiten del contexto enviado al modelo (`redact_sensitive_output`, activado por defecto). nsh no es un sandbox: para código que no se conozca, la frontera adecuada es un contenedor.

## Arquitectura

```text
┌─────────────────────────── nsh (binario Rust) ────────────────────────────┐
│                                                                            │
│  REPL (rustyline) ──┬── "!" ──► BashSession (PTY real, persistente)        │
│                     │        │  · cd/alias/export sobreviven              │
│                     │        │  · barrera de fin de comando con nonce      │
│                     │        │    aleatorio: exit code siempre real        │
│                     │                                                      │
│                     ├── "@" ──► resolvedor de rutas (verifica existencia,  │
│                     │        │  expande ~, autocompletado Tab)             │
│                     │                                                      │
│                     └── texto natural ──► Planner LLM (60 s, 3 reintentos) │
│                            │  contexto: cwd, ficheros del directorio,      │
│                     ◄──────┤  últimos comandos + exit codes, rutas @       │
│                     ▼                                                      │
│                 Política (policy.rs): Allow / Confirm / Deny               │
│                            ▼                                               │
│                  Aprobación humana ──► BashSession                         │
│                                                                            │
│  Broker MCP (hilo Tokio aislado, stdio, rmcp) ──► conectores opcionales    │
└────────────────────────────────────────────────────────────────────────────┘
```

Decisiones de diseño:

- **Sesión PTY real**, no `bash -c` por comando: protocolo de dos marcadores con nonce aleatorio que separa la salida del eco del prompt; el parser se resincroniza ante marcadores falsos o partidos entre lecturas.
- **Validación estricta de configuración**: proveedor, modelo, permisos y clave se verifican íntegramente al arranque.
- **Límites acotados**: 60 s por petición LLM con 3 reintentos, salidas truncadas antes de entrar al contexto, conectores MCP con tope de bytes y tiempo.
- **Plugins en caliente**: `/plugins install` escribe el bloque `[connectors.*]` y recarga el broker MCP sin reiniciar.
- **76 tests** (unitarios e integración sobre PTY): ANSI, salidas masivas, marcadores falsos, Ctrl+C, restauración de termios ante pánico.

Pila: `rustyline` · `portable-pty` · `rmcp` + Tokio · `ureq` · `serde`/`toml`. Sin agentic frameworks: 6.700 líneas de Rust revisables de principio a fin.

## Licencia

MIT.
