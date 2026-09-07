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

# El nonce NO se exporta. Identifica la sesión, pero no es un secreto frente
# a la propia Bash; execute verifica el final mediante una barrera adicional.
# Es readonly: si el usuario pudiera vaciarlo (`unset NSH_NONCE`), nsh esperaria
# eternamente un marcador que nunca llegaria y la sesion quedaria colgada.
NSH_NONCE='@@NONCE@@'
readonly NSH_NONCE
NSH_ID='boot'

# PS0 se expande DESPUES de leer el comando y ANTES de ejecutarlo.
# Reactiva el eco para que `read -p`, `npm init`, `apt` etc. muestren lo que
# teclea el usuario. La sustitucion no imprime nada. `command -p` por la misma
# razon que en __nsh_hook. Readonly: PS0 ejecuta codigo antes de cada comando,
# y reasignado por el usuario podria redefinir el hook o hacer `exec bash`.
PS0='$(command -p stty echo 2>/dev/null)'
readonly PS0

__nsh_hook() {
    # Sin `local`: una funcion llamada `local` (definible por el usuario)
    # sombrearia el builtin y dejaria vacio el codigo de salida.
    __nsh_s=$?
    # El hook debe emitir su marcador SIEMPRE, venga lo que venga del comando
    # anterior. `builtin printf` inmuniza contra funciones/alias que sombreen
    # printf (p. ej. `printf(){ :; }`) y `command -p` busca stty/base64 en el
    # PATH por defecto del sistema, ignorando funciones, alias y cambios de
    # PATH. Si base64 no existiera, el cwd llega vacio pero el marcador se
    # emite igualmente: nunca puede quedarse sin emitir.
    command -p stty -echo 2>/dev/null
    __nsh_cwd64=$(builtin printf %s "$PWD" | command -p base64 -w0)
    builtin printf '\033]777;nsh;%s;%s;%d;%s\007' \
        "$NSH_NONCE" \
        "${1:-$NSH_ID}" \
        "$__nsh_s" \
        "${__nsh_cwd64:0:5460}"
    return "$__nsh_s"
}

PROMPT_COMMAND=__nsh_hook
# Readonly: si el usuario pudiera quitar el hook (`unset PROMPT_COMMAND`,
# `unset -f __nsh_hook`, `PROMPT_COMMAND=...`), nsh dejaria de recibir
# marcadores y la sesion se colgaria esperando el fin de comando.
readonly PROMPT_COMMAND
readonly -f __nsh_hook

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
    /// false cuando la shell ya no puede sincronizarse: murió (EOF/`!exit`)
    /// o alguien la sustituyó (`!exec bash`, que pierde nonce y hook). El
    /// REPL consulta `alive()` para no seguir pintando prompts sobre un
    /// cadáver.
    alive: bool,
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
            alive: true,
            _rcfile: rcfile,
        };

        // 6. Consumir el marcador de arranque (id "boot").
        // Con timeout: si bash no existe o el rcfile falla, arrancar debe dar
        // error en vez de colgarse para siempre.
        session.drain_until("boot", false, Some(Duration::from_secs(15)))?;
        Ok(session)
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// true mientras la shell interna pueda sincronizarse con nsh. Tras
    /// `!exit`, `!exec bash` o un fallo de sincronía permanente devuelve false
    /// y el REPL debe cerrar en vez de seguir pintando prompts.
    pub fn alive(&self) -> bool {
        self.alive
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
        // Confirma que la shell esta viva y sincronizada. Con timeout: si el
        // usuario manipulo NSH_ID/NSH_NONCE (o hizo `exec` de otra shell), el
        // marcador no llega y hay que devolver el prompt con error, no colgarse.
        // Esta espera ocurre ANTES de entrar en raw mode: el terminal queda sano.
        // Si vence el timeout, la sincronía está rota de forma permanente (p. ej.
        // `!exec bash` sustituyó la shell y perdió nonce y hook): la sesión ya no
        // sirve y así se le comunica al REPL vía alive()=false.
        if let Err(e) = self.write_line(&format!("NSH_ID='{id}'")) {
            self.alive = false;
            return Err(e);
        }
        if let Err(e) = self.drain_until(&id, false, Some(Duration::from_secs(5))) {
            self.alive = false;
            return Err(e);
        }

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
        // Mantener un único camino de limpieza, incluso si write falla.
        let result = (|| {
            self.write_line(cmd)?;

            // FASE 4 — drenar hasta el marcador final. Sin timeout global: un comando
            // legitimo puede tardar (sleep, top, vim). La desincronizacion se detecta
            // por marcador ajeno (ver drain_until): cada comando solo puede emitir
            // el marcador de su propio id.
            let first = self.drain_until(&id, true, None)?.unwrap();
            // Un marcador en stdout no prueba que Bash haya vuelto al prompt.
            // Encolar una barrera con un id nuevo, que el comando anterior no
            // conocía. Bash solo puede ejecutarla cuando termina ese comando.
            let fence = format!("verify{:032x}", rand::rng().random::<u128>());
            self.write_line(&format!("__nsh_hook '{fence}'"))?;
            let mut verified = self.drain_until(&fence, true, None)?.unwrap();
            let mut output = first.output;
            output.extend_from_slice(&verified.output);
            verified.truncated |= first.truncated || output.len() > MAX_CAPTURE;
            if output.len() > MAX_CAPTURE {
                output.drain(..output.len() - MAX_CAPTURE);
            }
            verified.output = output;
            Ok(verified)
        })();
        if result.is_err() {
            self.alive = false;
        }

        stop.store(true, Ordering::Relaxed);
        let _ = handle.join();
        drop(_raw); // vuelve a modo cocido antes de imprimir nada

        result
    }

    /// Consume eventos hasta ver Finished con `id`.
    /// Si `capture` es true, imprime la salida en vivo y la acumula.
    /// `timeout` acota la espera (fases de armar/arranque). En FASE 4 es None,
    /// pero un marcador con id ajeno delata desincronizacion (p. ej. el comando
    /// reasigno NSH_ID) y aborta de inmediato en vez de esperar eternamente.
    fn drain_until(
        &mut self,
        id: &str,
        capture: bool,
        timeout: Option<Duration>,
    ) -> Result<Option<CommandResult>> {
        let mut out: Vec<u8> = Vec::new();
        let mut truncated = false;
        let stdout = std::io::stdout();
        let start = std::time::Instant::now();

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
                    if capture && !id.starts_with("verify") {
                        // En FASE 4 no puede llegar ningun otro marcador: el
                        // comando reasigno NSH_ID a mitad de ejecucion y ya no
                        // veremos el nuestro. Abortar aqui, con el terminal
                        // restaurado por RawGuard, en vez de colgarse.
                        bail!(
                            "la shell se desincronizó (marcador inesperado '{fid}'; ¿el comando modificó NSH_ID?). Reinicia nsh si los comandos dejan de responder"
                        );
                    }
                    // Fuera de captura (armar/arranque): marcador obsoleto, se ignora.
                }
                Err(RecvTimeoutError::Timeout) => {
                    if let Some(limit) = timeout {
                        if start.elapsed() > limit {
                            bail!(
                                "la shell no responde al marcador de sincronía (¿NSH_NONCE/NSH_ID manipulados? ¿`exec` de otra shell?). Reinicia nsh"
                            );
                        }
                    }
                    // Momento de atender SIGWINCH mientras corre `top` o `vim`.
                    if crate::winch_pending() {
                        let (r, c) = term::window_size();
                        self.resize(r, c);
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.alive = false;
                    bail!("la shell ha terminado");
                }
            }
        }
    }

    pub fn shutdown(&mut self) {
        let _ = self.write_line("exit");
    }
}

#[cfg(test)]
mod tests {
    use super::RCFILE_TEMPLATE;

    #[test]
    fn fallo_de_escritura_cierra_sesion_y_libera_forwarder() {
        use super::*;
        struct FallibleWriter {
            inner: Box<dyn Write + Send>,
            remaining: usize,
        }
        impl Write for FallibleWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.remaining == 0 {
                    return Err(std::io::ErrorKind::BrokenPipe.into());
                }
                self.remaining -= 1;
                self.inner.write(bytes)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.inner.flush()
            }
        }
        // Fallo al armar y fallo al enviar, después de arrancar el forwarder.
        for remaining in [0, 2] {
            let mut session = BashSession::start(false).unwrap();
            let inner = std::mem::replace(
                &mut *session.writer.lock().unwrap(),
                Box::new(std::io::sink()),
            );
            *session.writer.lock().unwrap() = Box::new(FallibleWriter { inner, remaining });
            assert!(session.execute("echo prueba").is_err());
            assert!(!session.alive());
            assert_eq!(Arc::strong_count(&session.writer), 1);
        }
    }

    /// El hook se emite con `builtin` y busca externos con `command -p`: sin
    /// eso, `!printf(){ :; }` o un cambio de PATH dejan el hook mudo y nsh
    /// espera eternamente un marcador que nunca llega (FASE 4 no tiene
    /// timeout a proposito). Y PS0 readonly: podria ejecutar codigo arbitrario
    /// antes de cada comando.
    #[test]
    fn hook_endurecido_contra_sombra_de_funciones() {
        assert!(RCFILE_TEMPLATE.contains("builtin printf"));
        assert!(RCFILE_TEMPLATE.contains("command -p stty"));
        assert!(RCFILE_TEMPLATE.contains("command -p base64"));
        assert!(RCFILE_TEMPLATE.contains("readonly PS0"));
        assert!(RCFILE_TEMPLATE.contains("readonly PROMPT_COMMAND"));
        assert!(RCFILE_TEMPLATE.contains("readonly -f __nsh_hook"));
    }

    /// El Base64 del cwd se corta a 5460 chars (4095 bytes decodificados): un
    /// PWD artificial gigante no puede superar el MAX_MARKER_LEN (8192) del
    /// parser y colgar la espera del marcador.
    #[test]
    fn cwd_del_marcador_acotado() {
        assert!(RCFILE_TEMPLATE.contains("${__nsh_cwd64:0:5460}"));
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
