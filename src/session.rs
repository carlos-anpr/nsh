/// Plantilla del rcfile. `@@NONCE@@` se sustituye en tiempo de ejecución.
pub const RCFILE_TEMPLATE: &str = r#"
# --- nsh rcfile: generado automaticamente, no editar ---

# El .bashrc del usuario NO se carga por defecto: starship, direnv y atuin
# pisan PROMPT_COMMAND y escriben en el terminal. Opt-in con --load-bashrc.
if [[ -n "$NSH_LOAD_BASHRC" && -f "$HOME/.bashrc" ]]; then
    source "$HOME/.bashrc"
fi

# Sin esto, readline inyecta \e[?2004h y \e[?2004l alrededor de cada prompt
# y ensucia el flujo que parsea nsh.
bind 'set enable-bracketed-paste off' 2>/dev/null

# Eco APAGADO mientras nsh escribe el comando (nsh ya lo ha mostrado en su prompt).
stty -echo

# El nonce NO se exporta: asi ningun proceso hijo puede leerlo con `printenv`
# y falsificar un marcador de fin de comando.
NSH_NONCE='@@NONCE@@'
NSH_ID='boot'

# PS0 se expande DESPUES de leer el comando y ANTES de ejecutarlo.
# Reactiva el eco para que `read -p`, `npm init`, `apt` etc. muestren lo que
# teclea el usuario. La sustitucion no imprime nada.
PS0='$(stty echo)'

__nsh_hook() {
    local s=$?          # OBLIGATORIO que sea la PRIMERA linea del cuerpo
    stty -echo          # vuelve a apagar el eco antes del siguiente comando
    printf '\033]777;nsh;%s;%s;%d;%s\007' \
        "$NSH_NONCE" \
        "$NSH_ID" \
        "$s" \
        "$(printf %s "$PWD" | base64 -w0)"
}

PROMPT_COMMAND=__nsh_hook

# nsh dibuja su propio prompt; bash no debe dibujar ninguno.
PS1=''
PS2=''
"#;

use anyhow::{Context, Result, bail};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
// DESVIACION del plan: usa `rand::Rng`. En rand 0.10 el trait de random_range
// se llama `RngExt` (y `thread_rng()` paso a `rng()`). Adaptacion minima.
use rand::RngExt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::NamedTempFile;

use crate::protocol::{Event, Parser};
use crate::term;

/// Tope de salida que se guarda en memoria (se conservan los ULTIMOS bytes,
/// que suelen ser los utiles). Al usuario se le muestra todo en vivo igualmente.
const MAX_CAPTURE: usize = 256 * 1024;

pub struct CommandResult {
    pub exit_code: i32,
    pub cwd: PathBuf,
    pub output: Vec<u8>,
    pub truncated: bool,
}

pub struct BashSession {
    master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    rx: Receiver<Event>,
    cwd: PathBuf,
    counter: u64,
    /// Se mantiene vivo para que el fichero no se borre mientras bash existe.
    _rcfile: NamedTempFile,
}

impl BashSession {
    pub fn start(load_bashrc: bool) -> Result<BashSession> {
        // 1. Nonce aleatorio de sesion.
        let nonce: String = {
            // DESVIACION del plan: rand 0.10 renombro thread_rng() -> rng()
            // y gen_range() -> random_range().
            let mut rng = rand::rng();
            (0..16)
                .map(|_| char::from(b"abcdef0123456789"[rng.random_range(0..16)]))
                .collect()
        };

        // 2. rcfile temporal con permisos 0600 (contiene el nonce).
        let mut rcfile = NamedTempFile::new().context("no se pudo crear el rcfile")?;
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(rcfile.path(), perms)?;
        }
        let content = RCFILE_TEMPLATE.replace("@@NONCE@@", &nonce);
        rcfile.write_all(content.as_bytes())?;
        rcfile.flush()?;

        // 3. Abrir la PTY con el tamano REAL del terminal.
        let (rows, cols) = term::window_size();
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        // 4. Lanzar bash.
        //    OJO: las opciones largas van ANTES de las cortas.
        //    `bash -i --noediting` es un error de uso.
        let mut cmd = CommandBuilder::new("bash");
        cmd.arg("--noprofile");
        cmd.arg("--rcfile");
        cmd.arg(rcfile.path().to_str().unwrap());
        cmd.arg("-i");
        cmd.env(
            "TERM",
            std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into()),
        );
        if load_bashrc {
            cmd.env("NSH_LOAD_BASHRC", "1");
        }
        if let Ok(dir) = std::env::current_dir() {
            cmd.cwd(dir);
        }

        let _child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave); // IMPRESCINDIBLE: si no, nunca llega EOF en el master.

        // 5. Hilo lector: PTY -> Parser -> canal. NUNCA escribe en stdout.
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let (tx, rx) = channel::<Event>();
        let nonce_for_thread = nonce.clone();

        std::thread::spawn(move || {
            let mut parser = Parser::new(&nonce_for_thread);
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        for ev in parser.push(&buf[..n]) {
                            if tx.send(ev).is_err() {
                                return;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            // Al salir se cierra tx -> el hilo principal detecta Disconnected.
        });

        let mut session = BashSession {
            master: pair.master,
            writer: Arc::new(Mutex::new(writer)),
            rx,
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
            counter: 0,
            _rcfile: rcfile,
        };

        // 6. Consumir el marcador de arranque (id "boot").
        session.drain_until("boot", false)?;
        Ok(session)
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    fn write_line(&self, s: &str) -> Result<()> {
        let mut w = self.writer.lock().unwrap();
        w.write_all(s.as_bytes())?;
        w.write_all(b"\n")?;
        w.flush()?;
        Ok(())
    }

    /// Valida la sintaxis ANTES de enviar. Si no, una comilla sin cerrar deja a
    /// bash esperando en PS2 y la sesion parece colgada.
    pub fn check_syntax(&self, cmd: &str) -> Result<()> {
        let out = std::process::Command::new("bash")
            .arg("-n")
            .arg("-c")
            .arg(cmd)
            .output()
            .context("no se pudo ejecutar bash -n")?;
        if !out.status.success() {
            let msg = String::from_utf8_lossy(&out.stderr);
            bail!("sintaxis invalida: {}", msg.trim());
        }
        Ok(())
    }

    pub fn execute(&mut self, cmd: &str) -> Result<CommandResult> {
        self.counter += 1;
        let id = format!("c{}", self.counter);

        // FASE 1 — "armar": fijar NSH_ID. Dispara un marcador con el id nuevo.
        // Confirma que la shell esta viva y sincronizada.
        self.write_line(&format!("NSH_ID='{}'", id))?;
        self.drain_until(&id, false)?;

        // FASE 2 — raw mode + reenvio de teclado, ANTES de enviar el comando.
        let _raw = term::RawGuard::enter();

        // ===============================================================
        // SOLO TEST (caso 24 del arnés). Nunca se activa en uso normal:
        // la variable no la pone nadie salvo `tests/integration.rs`.
        // Permite verificar que el hook de pánico del PASO 3 restaura los
        // termios del terminal real a modo cocido. El panic ocurre AQUÍ,
        // con el terminal ya en raw mode, que es el escenario peligroso.
        // ===============================================================
        if std::env::var("NSH_PANIC_TEST").as_deref() == Ok("1") {
            panic!("NSH_PANIC_TEST: panic deliberado dentro de RawGuard");
        }

        let stop = Arc::new(AtomicBool::new(false));
        let handle = spawn_stdin_forwarder(self.writer.clone(), stop.clone());

        // FASE 3 — enviar el comando EN CRUDO (nada de llaves).
        self.write_line(cmd)?;

        // FASE 4 — drenar hasta el marcador final.
        let result = self.drain_until(&id, true);

        stop.store(true, Ordering::Relaxed);
        let _ = handle.join();
        drop(_raw); // vuelve a modo cocido antes de imprimir nada

        result.map(|r| r.expect("drain_until con capture=true devuelve Some"))
    }

    /// Consume eventos hasta ver Finished con `id`.
    /// Si `capture` es true, imprime la salida en vivo y la acumula.
    fn drain_until(&mut self, id: &str, capture: bool) -> Result<Option<CommandResult>> {
        let mut out: Vec<u8> = Vec::new();
        let mut truncated = false;
        let stdout = std::io::stdout();

        loop {
            match self.rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Event::Output(bytes)) => {
                    let mut lock = stdout.lock();
                    let _ = lock.write_all(&bytes);
                    let _ = lock.flush();
                    if capture {
                        out.extend_from_slice(&bytes);
                        if out.len() > MAX_CAPTURE {
                            let exceso = out.len() - MAX_CAPTURE;
                            out.drain(..exceso);
                            truncated = true;
                        }
                    }
                }
                Ok(Event::Finished {
                    id: fid,
                    exit_code,
                    cwd,
                }) => {
                    if fid == id {
                        self.cwd = cwd.clone();
                        return Ok(if capture {
                            Some(CommandResult {
                                exit_code,
                                cwd,
                                output: out,
                                truncated,
                            })
                        } else {
                            None
                        });
                    }
                    // Marcador de otro comando: obsoleto, se ignora.
                }
                Err(RecvTimeoutError::Timeout) => {
                    // Momento de atender SIGWINCH mientras corre `top` o `vim`.
                    if crate::winch_pending() {
                        let (r, c) = term::window_size();
                        self.resize(r, c);
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    bail!("la shell ha terminado");
                }
            }
        }
    }

    pub fn shutdown(&mut self) {
        let _ = self.write_line("exit");
    }
}

/// Reenvia el teclado real hacia la PTY mientras corre un comando.
///
/// Usa poll() con timeout en vez de un read() bloqueante porque un read()
/// bloqueado NO se puede cancelar: el hilo seguiria vivo tras acabar el comando
/// y robaria la primera tecla del siguiente prompt.
fn spawn_stdin_forwarder(
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buf = [0u8; 1024];
        while !stop.load(Ordering::Relaxed) {
            let mut pfd = libc::pollfd {
                fd: 0,
                events: libc::POLLIN,
                revents: 0,
            };
            let r = unsafe { libc::poll(&mut pfd, 1, 50) };
            if r <= 0 {
                continue;
            }
            let n = unsafe { libc::read(0, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n <= 0 {
                break;
            }
            let mut w = writer.lock().unwrap();
            if w.write_all(&buf[..n as usize]).is_err() {
                break;
            }
            let _ = w.flush();
        }
    })
}
