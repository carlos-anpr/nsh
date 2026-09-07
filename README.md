<div align="center">

# nsh

### *Tu terminal, con cerebro*

[![Rust](https://img.shields.io/badge/Rust-2024-dea584?style=flat-square&logo=rust&logoColor=black)](https://www.rust-lang.org)
[![MCP](https://img.shields.io/badge/MCP-Model_Context_Protocol-7c3aed?style=flat-square)](https://modelcontextprotocol.io)
[![LLM](https://img.shields.io/badge/LLM-Anthropic_%7C_OpenAI_%7C_Ollama-ff6b6b?style=flat-square)](https://ollama.com)
[![License](https://img.shields.io/badge/License-MIT-339933?style=flat-square)](LICENSE)

**Una shell en Rust que entiende lenguaje natural, planifica el comando exacto y te pregunta antes de ejecutar.**

</div>

---

## Qué es

nsh envuelve una sesión de Bash real y persistente (`cd`, `alias` y `export` sobreviven entre comandos) y añade dos formas nuevas de hablar con ella:

```text
nsh ~/Proyectos/app ❯ !ls -la logs/
nsh ~/Proyectos/app ❯ busca los errores 500 de hoy y dime qué IP los concentra
nsh ~/Proyectos/app ❯ resumen de @informe.pdf
nsh ~/Proyectos/app ❯ /why
```

- **`!comando`** — Bash de toda la vida, sin filtros ni LLM.
- **Texto natural** — el LLM planifica el comando y tú lo apruebas.
- **`@fichero`** — referencia ficheros reales con autocompletado Tab.
- **`/why`** — el LLM interpreta la última salida.

![Demo de nsh](docs/demo/nsh-demo.gif)

## Instalación (Linux)

```bash
git clone https://github.com/cursospotiapp/nsh.git
cd nsh && cargo build --release
cargo install --path .
```

Configura tu modelo en `~/.config/nsh/config.toml` (permisos `600`, obligatorios):

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

# Ollama — 100 % local, sin API key
[providers.ollama]
base_url = "http://localhost:11434/v1"
api = "openai"
api_key = "ollama"
models = ["qwen3:8b"]
```

Para no guardar la clave en el fichero: `api_key_env = "MI_VARIABLE"`.
</details>

```bash
nsh
```

Sin config, nsh funciona igual como shell: los `!comandos` no necesitan LLM.

## Cómo se usa

| | Ejemplo | Qué pasa |
|---|---|---|
| `!comando` | `!find . -name "*.log"` | Ejecuta en la sesión, sin política |
| Texto natural | `qué fichero pesa más de 100 MB` | El LLM propone el comando; pulsas `[e]jecutar`, `[c]ancelar` o `[m]odificar` |
| `@fichero` | `resumen de @contrato.pdf` | Verifica la ruta y la pasa al modelo (con plugin `documentos`: pdf/docx/xlsx) |
| `/why` | `/why` | El LLM explica la última salida y el siguiente paso razonable |
| `/fix` | `/fix` | Reintenta el último comando fallido con el error en el contexto |
| `/yolo` | `/yolo` | Toggle de aprobación automática |
| `/models` | `/models` | Cambia de modelo en caliente |
| `/plugins` | `/plugins install documentos` | Instala capacidades opcionales sin reiniciar |

## Seguridad: un LLM propone, tú dispones

Todo comando planificado pasa por una **política escrita en Rust** (lexer de Bash propio, no del prompt). Ante la duda, siempre restrictiva:

| Situación | `confirm` (defecto) | `yolo` |
|---|---|---|
| Lecturas y búsquedas | ejecuta | ejecuta |
| Escrituras y borrados normales | confirma | ejecuta |
| `chmod 777`, mutaciones del sistema, borrado masivo | confirma | **confirma** |
| Zonas de sistema, escalada de privilegios, `curl … \| sh` | **deniega** | **deniega** |

Los `!` tuyos nunca pasan por política. Las salidas que tocan rutas sensibles (`~/.ssh`, credenciales) no se envían al modelo (`redact_sensitive_output`, activado por defecto). nsh no es un sandbox: para código que no confías, contenedor.

## Debajo del capó

- **Sesión PTY real**, no `bash -c` por comando: protocolo de dos marcadores con nonce aleatorio separa la salida del eco del prompt; el exit code es siempre el real y el parser se resincroniza ante marcadores falsos o partidos.
- **Fail-fast**: el config se valida entero al arrancar (permisos, modelo, proveedor). Nada de arrancar a medias.
- **Límites por todas partes**: 60 s por petición LLM con 3 reintentos, salidas truncadas antes de entrar al contexto, conectores MCP con tope de bytes y tiempo.
- **Plugins en caliente**: `/plugins install` escribe el bloque `[connectors.*]` y recarga el broker MCP sin reiniciar.
- **76 tests** (unitarios + integración sobre PTY): ANSI, salidas masivas, marcadores falsos, Ctrl+C, panic que restaura el terminal.

Pila: `rustyline` · `portable-pty` · `rmcp` + Tokio · `ureq` · `serde`/`toml`. Sin agentic frameworks: 6.700 líneas de Rust que se leen en una tarde.

## Licencia

MIT.
