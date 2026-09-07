<div align="center">

# nsh

### *Tu terminal, con cerebro*

[![Rust](https://img.shields.io/badge/Rust-2024-dea584?style=flat-square&logo=rust&logoColor=black)](https://www.rust-lang.org)
[![Tokio](https://img.shields.io/badge/Tokio-runtime-0adb76?style=flat-square&logo=tokio&logoColor=black)](https://tokio.rs)
[![MCP](https://img.shields.io/badge/MCP-Model_Context_Protocol-7c3aed?style=flat-square)](https://modelcontextprotocol.io)
[![LLM](https://img.shields.io/badge/LLM-Anthropic_%7C_OpenAI_%7C_Ollama-ff6b6b?style=flat-square)](https://ollama.com)
[![License](https://img.shields.io/badge/License-MIT-339933?style=flat-square)](LICENSE)

**Una shell interactiva en Rust que entiende lo que le pides en lenguaje natural, planifica el comando exacto y te pregunta antes de ejecutar.**

*Nada de agentic frameworks. Nada de magia. Un binario, una terminal y un modelo de lenguaje.*

</div>

---

## Qué es nsh

nsh es una **shell asistida por LLM**: envuelve una sesión de Bash real y persistente y le añade dos superpoderes:

```text
nsh ~/Proyectos/app ❯ !ls -la logs/
nsh ~/Proyectos/app ❯ busca los errores 500 de hoy y dime qué IP los concentra
nsh ~/Proyectos/app ❯ resumen de @informe.pdf
nsh ~/Proyectos/app ❯ /why
```

- **`!comando`** — Ejecuta Bash puro, con `cd`, `alias` y `export` **persistentes entre comandos** (una sesión viva, no un `bash -c` por comando).
- **Texto natural** — Describe lo que quieres en castellano o inglés; un LLM planifica el comando exacto y tú decides si se ejecuta.
- **`@fichero`** — Referencia ficheros reales con autocompletado Tab; se verifican antes de llamar al modelo.
- **`/why`** — Pregunta al LLM que interprete la última salida: qué significa, qué hacer después.

### Demo

![Demo de nsh: comandos Bash con !, petición al LLM, aprobación del comando planificado y explicación con /why](docs/demo/nsh-demo.gif)

> *15 segundos con datos ficticios:* `!ls` explora los logs; el LLM construye un `grep` que filtra los errores 500 de `/api/pagos`, excluye las peticiones de prueba y agrupa por IP; `/why` interpreta qué IP concentra el incidente. La demo se regenera con [docs/demo/generate.py](docs/demo/generate.py) — reproducible, no un montaje.

### Por qué es distinto

| | nsh | Un script de Bash | Un agente autónomo |
|---|---|---|---|
| Entiende lenguaje natural | ✔ | ✖ | ✔ |
| La sesión Bash persiste (`cd`, `alias`, `export`) | ✔ | ✖ | rara vez |
| Tú apruebas cada comando antes de que se ejecute | ✔ | — | ✖ |
| Un solo binario, sin framework | ✔ | ✔ | ✖ |

---

## Cómo se usa, en palabras llanas

Imagina que abres la terminal y no recuerdas cómo se hacía algo. Con nsh tienes tres formas de pedirlo:

**1. La forma clásica.** Escribe `!` delante y es Bash de toda la vida, tal cual:

```text
nsh ~ ❯ !find . -name "*.log" -mtime -1
```

Todo lo que corre con `!` es tuyo: nsh no lo filtra ni lo toca.

**2. La forma hablada.** Escribe lo que quieres conseguir, sin `!`:

```text
nsh ~ ❯ ayúdame a encontrar qué fichero de este proyecto pesa más de 100 MB
```

nsh se lo cuenta al modelo de lenguaje, el modelo propone el comando exacto, y nsh te lo enseña antes de hacer nada:

```text
  ▸ find . -type f -size +100M -exec du -h {} + | sort -rh
  [e]jecutar  [c]ancelar  [m]odificar
```

Pulsas `e` y se ejecuta en la misma sesión. Pulsas `c` y no pasa nada. Pulsas `m` y escribes el comando tú mismo.

**3. La forma curiosa.** Tras cualquier resultado, escribe `/why`:

```text
nsh ~ ❯ /why
```

y el modelo te explica qué significa esa salida y cuál sería el siguiente paso razonable. Perfecto para aprender, depurar o cuando llevas horas delante de un log y ya no ves nada.

---

## Puesta a cero (10 minutos)

### 1 · Requisitos

| Necesitas | Para qué | Cómo comprobarlo |
|---|---|---|
| **Rust 1.85+** (con Cargo) | Compilar el binario | `cargo --version` |
| **Bash** | La shell interna (presente en cualquier Linux/macOS) | `bash --version` |
| Una **API key** de un proveedor LLM | El modo lenguaje natural (sin ella, `!comandos` siguen funcionando) | — |

### 2 · Clona y compila

```bash
git clone https://github.com/cursospotiapp/nsh.git
cd nsh
cargo build --release
```

El binario queda en `target/release/nsh`. Puedes instalarlo en tu PATH:

```bash
cargo install --path .
```

### 3 · Configura tu modelo

nsh lee su configuración de `~/.config/nsh/config.toml`. Crea el directorio y el fichero:

```bash
mkdir -p ~/.config/nsh
chmod 700 ~/.config/nsh
```

Crea `~/.config/nsh/config.toml` con **uno** de estos bloques, según tu proveedor. Importante: el fichero debe tener permisos `600` — nsh lo comprueba en cada arranque y se niega a arrancar si es legible por otros:

```bash
chmod 600 ~/.config/nsh/config.toml
```

<details>
<summary><b>Z.AI (GLM)</b> — API estilo Anthropic</summary>

```toml
model = "zai/glm-4.7"

[providers.zai]
base_url = "https://api.z.ai/api/anthropic/v1"
api = "anthropic"
api_key = "TU_API_KEY"
models = ["glm-4.7"]
```
</details>

<details>
<summary><b>Anthropic (Claude)</b></summary>

```toml
model = "anthropic/claude-sonnet-4-5"

[providers.anthropic]
base_url = "https://api.anthropic.com"
api = "anthropic"
api_key = "TU_API_KEY"
models = ["claude-sonnet-4-5"]
```
</details>

<details>
<summary><b>OpenAI (o cualquier API compatible: LM Studio, vLLM, …)</b></summary>

```toml
model = "openai/gpt-4o"

[providers.openai]
base_url = "https://api.openai.com/v1"
api = "openai"
api_key = "TU_API_KEY"
models = ["gpt-4o"]
```

Para un servidor local con **LM Studio** o **vLLM**, cambia `base_url` por el del servidor y pon el nombre del modelo que sirva.
</details>

<details>
<summary><b>Ollama</b> — 100 % local, sin API key</summary>

```toml
model = "ollama/qwen3:8b"

[providers.ollama]
base_url = "http://localhost:11434/v1"
api = "openai"
api_key = "ollama"
models = ["qwen3:8b"]
```
</details>

<details>
<summary><b>Consejo</b> — la clave fuera del fichero</summary>

Si prefieres no guardar la clave en el config, usa `api_key_env` y nsh la leerá de esa variable de entorno:

```toml
[security]
approval = "confirm"

[providers.zai]
base_url = "https://api.z.ai/api/anthropic/v1"
api = "anthropic"
api_key_env = "ZAI_API_KEY"
models = ["glm-4.7"]
```
</details>

### 4 · Arranca

```bash
nsh
```

Verás el prompt:

```text
nsh ~ ❯
```

Escribe `!pwd`, prueba una petición en lenguaje natural y listo. Para salir, `/exit`.

---

## Comandos internos

| Comando | Qué hace |
|---|---|
| `!<comando>` | Ejecuta Bash directo, sin filtros ni LLM |
| `texto natural` | El LLM planifica un comando; tú lo apruebas |
| `/models` · `/model <nombre>` | Lista y cambia el modelo activo en caliente |
| `/fix` | Reintenta el último comando fallido: el LLM recibe el error y propone corrección |
| `/why` | El LLM interpreta la última salida: qué pasó y qué hacer |
| `/yolo` (`on`/`off`) | Alterna aprobación automática (ver seguridad) |
| `/plugins list` · `install <n>` · `remove <n>` | Gestiona capacidades opcionales |
| `/exit` · `/quit` | Sale de nsh |

---

## Seguridad: el comando nunca se ejecuta solo

nsh se diseñó bajo una premisa: **un LLM propone, tú dispones.**

Toda petición en lenguaje natural pasa por una política de clasificación escrita en Rust que decide **antes de preguntarte**:

| Situación | Modo `confirm` (defecto) | Modo `yolo` |
|---|---|---|
| Lecturas, búsquedas, tuberías de lectura | ejecuta | ejecuta |
| Escrituras y borrados normales, redirecciones, `tee`, globs | confirma | ejecuta |
| `chmod 777`, mutación del sistema (paquetes, servicios), borrado masivo (`find -delete`) | confirma | **confirma** |
| Zonas de sistema (`/etc`, `/usr`…), escalada de privilegios, `mkfs`, `rm -rf /`, `curl … \| sh` | **deniega** | **deniega** |

Los detalles que importan:

- **Política en Rust, no en el prompt.** La clasificación usa un lexer de Bash propio; lo que el modelo "prometa" no cuenta. Ante la duda, siempre hacia el lado restrictivo.
- **Cero permiso, cero confianza.** Los comandos `!` del usuario nunca pasan por política — son tus manos. La política solo vela los comandos planificados por el LLM.
- **Secretos fuera del contexto.** Cuando el resultado de un comando toca rutas sensibles (`~/.ssh`, credenciales, tokens), `nsh` lo omite de las muestras automáticas que se envían al modelo — siguiendo también enlaces simbólicos. Configurable con `redact_sensitive_output` (defecto: activado).
- **Sandbox honesto.** nsh no pretende aislar código hostil: la frontera real es un contenedor. El README técnico del código lo dice sin adornos, y lo repetimos aquí.

## Arquitectura, para quien mira debajo del capó

```text
┌─────────────────────────── nsh (binario Rust) ────────────────────────────┐
│                                                                            │
│  REPL (rustyline) ──┬── "!" ──► BashSession (PTY real, persistente)        │
│                     │        │  · cd/alias/export sobreviven              │
│                     │        │  · barrera de fin de comando con nonce      │
│                     │        │    aleatorio: el exit code es el real,      │
│                     │        │    no un marcador prematuro                │
│                     │                                                      │
│                     ├── "@" ──► resolvedor de rutas (verifica existencia,  │
│                     │        │  expande ~, fuzzy-completion con Tab)       │
│                     │                                                      │
│                     └── texto natural                                     │
│                            ▼                                              │
│                    Planner LLM (60 s timeout, 3 reintentos)               │
│                            │  contexto: cwd, ficheros reales del dir,      │
│                     ◄──────┤  últimos comandos + exit codes,               │
│                     │      │  rutas @ verificadas                          │
│                     ▼                                                      │
│                 Política (policy.rs): lexer + clasificación por acción     │
│                     Allow / Confirm / Deny                                 │
│                            ▼                                               │
│                  Aprobación humana ──► BashSession                         │
│                                                                            │
│  Broker MCP (hilo Tokio aislado, stdio, rmcp) ──► conectores opcionales    │
│                                                                            │
└────────────────────────────────────────────────────────────────────────────┘
```

Decisiones de diseño que me importan:

- **Sesión PTY real, no `bash -c` por comando.** Cada comando corre en una PTY pilotada con hilos de lectura y un protocolo de **dos marcadores en banda con nonce aleatorio** que separa la salida del comando del eco del prompt. Un marcador prematuro o falso no puede hacerse pasar por el fin de un comando, y el parser se resincroniza ante marcadores partidos entre lecturas. (27 tests solo de protocolo e integración PTY).
- **Fail-fast de configuración.** El config TOML se valida entero al arrancar: proveedor inexistente, modelo no listado, permisos distintos de `0600` o `api_key` ausente abortan con un error accionable. Nada de arrancar a medias.
- **Timeouts y límites por todas partes.** 60 s por petición LLM (lectura incluida) con hasta 3 reintentos; salida de comandos truncada antes de entrar al contexto; conectores MCP con tope de bytes y de tiempo.
- **Plugins sin reinicios.** `/plugins install documentos` comprueba el runtime, escribe el bloque `[connectors.*]` preservando permisos `0600` y recarga el broker MCP **en caliente**.
- **76 tests** entre unitarios e integración PTY (73 sin red + 3 de API real), incluidos: ANSI, salidas masivas truncadas, marcadores falsos, Ctrl+C en seco, panic que restaura el terminal (termios) y cierre limpio.

### Pila

`rustyline` (REPL y autocompletado fuzzy) · `portable-pty` (sesión Bash persistente) · `rmcp` + Tokio (broker MCP stdio) · `ureq` (clientes LLM Anthropic/OpenAI, síncronos y pequeños a propósito) · `serde`/`toml` (configuración validada) · `shell-words` + lexer propio (política de comandos)

### ¿Por qué no un agentic framework?

Porque el 90 % del valor de "una shell con IA" es **cablear bien la parte aburrida**: PTY, sincronización, permisos, contexto mínimo y veraz, y una política que no se pueda sobornar con un prompt bonito. Eso en Rust son 6.700 líneas que se leen en una tarde — y que aquí tienen test propio. El "framework" que queda es el que no existe: menos código, menos superficie, menos sorpresas.

---

## Plugins: capacidades opcionales

El núcleo siempre funciona. Las extras se instalan con un comando y sin editar ficheros:

```text
nsh ~ ❯ /plugins list
  Plugins disponibles:
    documentos  lee pdf/docx/xlsx vía MarkItDown (uvx)  [no instalado]
nsh ~ ❯ /plugins install documentos
  ✓ plugin 'documentos' instalado. Ya puedes usarlo.
```

**`documentos`** — pide `resumen de @contrato.pdf` (o una ruta desnuda, con espacios incluso) y nsh lo convierte a Markdown **antes** de llamar al LLM, mediante un conector MCP basado en MarkItDown. La conversión nunca va al system prompt: se inyecta como datos externos delimitados. Requiere `uvx` (viene con [uv](https://docs.astral.sh/uv/)).

---

## Preguntas honestas

**¿Necesito API key para usarlo?**
No. Sin config, nsh arranca en modo shell: los `!comandos` funcionan igual, con persistencia de sesión incluida. El modo LLM se desbloquea con una clave de [Z.AI](https://z.ai), [Anthropic](https://console.anthropic.com), [OpenAI](https://platform.openai.com) o [Ollama](https://ollama.com) en local.

**¿Manda mi terminal a la nube?**
Al modelo van: tu petición, el directorio actual, los nombres de ficheros del directorio (no su contenido), los últimos comandos con su código de salida y las rutas `@` que verificaste. La salida del último comando solo se envía con `/fix` y `/why`, y las rutas sensibles se omiten si `redact_sensitive_output` está activo (defecto).

**¿Puede el LLM romper mi sistema?**
El comando planificado pasa una política en Rust con lista corta de prohibidos absolutos (zonas de sistema, escalada de privilegios, `rm -rf /`, descarga-y-ejecuta) y confirmación humana para todo lo que no es lectura. Aun así, para código que no conoces o no confías, la frontera honesta es ejecutar nsh en un contenedor — como harías con cualquier herramienta que ejecuta Bash.

**¿Funciona en macOS?**
Sí; usa PTY estándar de Unix. Los tests de integración corren en Linux; en macOS debería compilar y funcionar igual.

---

## Licencia

MIT — cada decisión de diseño está tomada a conciencia y documentada en el propio código.