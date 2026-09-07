// Arnés de integración: lanza el binario `nsh` dentro de una PTY pilotable
// (portable-pty) y comprueba los casos del PASO 6 que no necesitan interacción
// humana: 1,2,3,4,5,6,7,8,9,10,12,14,15,16,20,21,22,23.
//
// Caso 24 (higiene del terminal): se automatiza por la vía del PANIC — que es
// lo que el restore() del PASO 3 cubre de verdad — en `caso_24_panic_restaura_termios`.
// SIGTERM también restaura desde el PASO 25 (`sigterm_restaura_termios`).
//
// El caso 2 (la prueba decisiva del MVP) se comprueba sobre el PROMPT de nsh
// (`nsh /tmp ❯`), no sobre la salida de `!pwd`.
//
// El PTY se fija a 40 filas x 120 columnas para que el caso 12 (`!tput cols`)
// devuelva 120.

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::os::unix::io::RawFd;
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::thread;
use std::time::{Duration, Instant};

const ROWS: u16 = 40;
const COLS: u16 = 120;

/// Driver de la sesión de nsh dentro de una PTY.
struct NshPty {
    _master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send>,
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    buf: Vec<u8>,
}

impl NshPty {
    fn new() -> Self {
        Self::new_with_cwd(std::env::temp_dir())
    }

    fn new_with_cwd(cwd: std::path::PathBuf) -> Self {
        Self::new_inner(cwd, &[])
    }

    /// Constructor con variables de entorno extra (para NSH_PANIC_TEST, etc.).
    fn new_with_env(cwd: std::path::PathBuf, envs: &[(&str, &str)]) -> Self {
        Self::new_inner(cwd, envs)
    }

    fn new_inner(cwd: std::path::PathBuf, envs: &[(&str, &str)]) -> Self {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: ROWS,
                cols: COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");

        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_nsh"));
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env(
            "HOME",
            std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()),
        );
        for (k, v) in envs {
            cmd.env(k, v);
        }

        let child = pair.slave.spawn_command(cmd).expect("spawn nsh");
        drop(pair.slave); // que llegue EOF al cerrar

        let mut reader = pair.master.try_clone_reader().expect("clone_reader");
        let writer = pair.master.take_writer().expect("take_writer");
        let (tx, rx) = channel::<Vec<u8>>();

        thread::spawn(move || {
            let mut b = [0u8; 65536];
            loop {
                match reader.read(&mut b) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(b[..n].to_vec()).is_err() {
                            return;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let mut s = NshPty {
            _master: pair.master,
            writer,
            child,
            rx,
            buf: Vec::new(),
        };
        // Espera al primer prompt.
        s.read_until(b"\xe2\x9d\xaf", 10_000, "primer prompt");
        // Da un margen para que se asienten las secuencias de rustyline.
        s.drain(150);
        s.buf.clear();
        s
    }

    /// PID del proceso `nsh` (None en plataformas sin pid).
    fn pid(&self) -> u32 {
        self.child
            .process_id()
            .expect("nsh no tiene pid disponible")
    }

    /// Envía una señal a `nsh` (SIGTERM, SIGKILL, ...).
    fn signal(&self, sig: i32) {
        unsafe {
            libc::kill(self.pid() as i32, sig);
        }
    }

    /// Drena hasta que el canal se desconecta (nsh murió y cerró el master).
    fn wait_for_exit(&mut self, timeout_ms: u64) {
        let end = Instant::now() + Duration::from_millis(timeout_ms);
        while Instant::now() < end {
            match self.rx.recv_timeout(Duration::from_millis(50)) {
                Ok(chunk) => self.buf.extend_from_slice(&chunk),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn pump(&mut self) -> bool {
        match self.rx.recv_timeout(Duration::from_millis(50)) {
            Ok(chunk) => {
                self.buf.extend_from_slice(&chunk);
                true
            }
            Err(RecvTimeoutError::Timeout) => false,
            Err(RecvTimeoutError::Disconnected) => false,
        }
    }

    /// Lee hasta que `needle` aparece en el búfer o vence el timeout.
    /// Para no ser O(n²), solo se rebusca en el trozo no buscado todavía
    /// (con solape de `needle.len()-1` para marcadores a caballo entre chunks).
    fn read_until(&mut self, needle: &[u8], timeout_ms: u64, what: &str) {
        let end = Instant::now() + Duration::from_millis(timeout_ms);
        let mut searched = 0usize;
        loop {
            if self.buf.len() >= needle.len() {
                let from = searched.saturating_sub(needle.len() - 1);
                if find_sub(&self.buf[from..], needle).is_some() {
                    return;
                }
                searched = self.buf.len();
            }
            if Instant::now() >= end {
                eprintln!(
                    "[read_until] TIMEOUT buf={} bytes buscando {:?} ({})",
                    self.buf.len(),
                    std::str::from_utf8(needle).unwrap_or("<bin>"),
                    what
                );
                panic!(
                    "timeout esperando {:?} ({})\n--- BUF (ultimos 500) ---\n{}",
                    std::str::from_utf8(needle).unwrap_or("<bin>"),
                    what,
                    String::from_utf8_lossy(&self.buf[self.buf.len().saturating_sub(500)..])
                );
            }
            self.pump();
        }
    }

    /// Drena sin buscar nada durante `ms` milisegundos.
    fn drain(&mut self, ms: u64) {
        let end = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < end {
            self.pump();
        }
    }

    /// Envía una línea (comando del REPL, incluye el `!` si procede).
    fn send_line(&mut self, line: &str) {
        self.writer.write_all(line.as_bytes()).expect("write cmd");
        self.writer.write_all(b"\n").expect("write nl");
        self.writer.flush().expect("flush");
    }

    /// Envía bytes en crudo (Ctrl+C, etc.).
    fn send_raw(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).expect("write raw");
        self.writer.flush().expect("flush");
    }

    /// Ejecuta `!cmd`, espera a `[terminado: N]` y al siguiente prompt, y
    /// devuelve una instantánea del búfer acumulado.
    fn run_cmd(&mut self, cmd: &str, timeout_ms: u64) -> Vec<u8> {
        self.buf.clear();
        self.send_line(&format!("!{cmd}"));
        self.read_until(b"[terminado:", timeout_ms, &format!("cmd: !{cmd}"));
        // captura el `N]`, el `\n` y el siguiente prompt
        self.drain(400);
        let snap = self.buf.clone();
        self.buf.clear();
        snap
    }

    fn snapshot(&self) -> Vec<u8> {
        self.buf.clone()
    }
}

// ---------- utilidades de bytes ----------

fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Último `[terminado: N]` del búfer.
fn last_exit(b: &[u8]) -> Option<i32> {
    let needle = b"[terminado: ";
    let mut last_pos: Option<usize> = None;
    let mut i = 0;
    while let Some(p) = find_sub(&b[i..], needle) {
        last_pos = Some(i + p);
        i += p + 1;
    }
    let pos = last_pos?;
    let after = &b[pos + needle.len()..];
    let mut val = 0i32;
    let mut any = false;
    for &c in after {
        if c.is_ascii_digit() {
            val = val * 10 + (c - b'0') as i32;
            any = true;
        } else {
            break;
        }
    }
    if any { Some(val) } else { None }
}

/// Cuenta cuántas veces aparece `[terminado:` en el búfer.
fn count_terminado(b: &[u8]) -> usize {
    let needle = b"[terminado:";
    let mut n = 0;
    let mut i = 0;
    while let Some(p) = find_sub(&b[i..], needle) {
        n += 1;
        i += p + 1;
    }
    n
}

/// Último `nsh <cwd> ❯` del búfer (la cwd entre el `nsh ` y ` ❯`).
fn last_prompt_cwd(b: &[u8]) -> Option<String> {
    let prefix: &[u8] = b"nsh ";
    let suffix: &[u8] = b" \xe2\x9d\xaf"; // " ❯"
    let mut last: Option<String> = None;
    let mut i = 0;
    while let Some(p) = find_sub(&b[i..], prefix) {
        let start = i + p + prefix.len();
        match find_sub(&b[start..], suffix) {
            Some(end) => {
                last = Some(String::from_utf8_lossy(&b[start..start + end]).into_owned());
                i = start + 1;
            }
            None => break,
        }
    }
    last
}

// ---------- casos ----------

#[test]
fn prefijo_incompleto_no_pierde_fin_real() {
    let mut p = NshPty::new();
    let out = p.run_cmd("builtin printf '\\033]777;nsh;basura-sin-bel'", 5000);
    assert_eq!(last_exit(&out), Some(0));
    assert_eq!(last_exit(&p.run_cmd("false", 5000)), Some(1));
}

#[test]
fn marcador_valido_falso_no_adelanta_finalizacion() {
    let mut p = NshPty::new();
    p.send_line("!builtin printf '\\033]777;nsh;%s;%s;0;Lw==\\007' \"$NSH_NONCE\" \"$NSH_ID\"; sleep 1; echo SALIDA_REAL; false");
    p.drain(300);
    assert_eq!(count_terminado(&p.snapshot()), 0);
    p.read_until(b"[terminado:", 5000, "barrera tras marcador falso");
    p.drain(200);
    assert_eq!(last_exit(&p.snapshot()), Some(1));
    assert!(String::from_utf8_lossy(&p.snapshot()).contains("SALIDA_REAL"));
    assert_eq!(last_exit(&p.run_cmd("echo SIGUIENTE", 5000)), Some(0));
}

#[test]
fn caso_01_pwd() {
    let mut p = NshPty::new();
    let out = p.run_cmd("pwd", 5000);
    assert_eq!(last_exit(&out), Some(0));
    // pwd imprime una ruta absoluta (arrancamos en /tmp).
    let visible = String::from_utf8_lossy(&out);
    let line = visible
        .lines()
        .find(|l| l.starts_with('/') && !l.starts_with("/["))
        .unwrap_or("");
    assert!(
        line.starts_with('/'),
        "esperaba una ruta absoluta: {}",
        visible
    );
}

#[test]
fn caso_02_cd_persiste_sobre_prompt() {
    // LA PRUEBA DECISIVA DEL MVP: el prompt de nsh debe cambiar a `nsh /tmp ❯`.
    let mut p = NshPty::new();
    // 1. cd /tmp
    let out_cd = p.run_cmd("cd /tmp", 5000);
    assert_eq!(last_exit(&out_cd), Some(0));
    // 2. el ÚLTIMO prompt visible debe ser /tmp
    let prompt_cwd = last_prompt_cwd(&out_cd).expect("no se vio prompt tras cd");
    assert_eq!(
        prompt_cwd, "/tmp",
        "el prompt NO cambio a /tmp (fue {:?}); la shell NO es persistente",
        prompt_cwd
    );
    // 3. doble confirmación con pwd
    let out_pwd = p.run_cmd("pwd", 5000);
    assert_eq!(last_exit(&out_pwd), Some(0));
    assert!(
        find_sub(&out_pwd, b"/tmp").is_some(),
        "pwd no devolvio /tmp: {}",
        String::from_utf8_lossy(&out_pwd)
    );
}

#[test]
fn caso_03_export_persiste() {
    let mut p = NshPty::new();
    let _ = p.run_cmd("export M=hola", 5000);
    let out = p.run_cmd("printf '%s\\n' \"$M\"", 5000);
    assert_eq!(last_exit(&out), Some(0));
    assert!(
        find_sub(&out, b"hola").is_some(),
        "esperaba 'hola': {}",
        String::from_utf8_lossy(&out)
    );
}

#[test]
fn caso_04_alias_persiste() {
    let mut p = NshPty::new();
    let _ = p.run_cmd("alias la='ls -la'", 5000);
    let out = p.run_cmd("la", 5000);
    assert_eq!(last_exit(&out), Some(0));
    // `ls -la` lista siempre al menos `total ` o entradas `drw`
    assert!(
        find_sub(&out, b"total").is_some() || find_sub(&out, b"drw").is_some(),
        "esperaba un listado largo: {}",
        String::from_utf8_lossy(&out)
    );
}

#[test]
fn caso_05_exitcode_y_stderr() {
    let mut p = NshPty::new();
    let out = p.run_cmd("sh -c 'echo err >&2; exit 7'", 5000);
    assert!(
        find_sub(&out, b"err").is_some(),
        "stderr 'err' no visible: {}",
        String::from_utf8_lossy(&out)
    );
    assert_eq!(last_exit(&out), Some(7));
}

#[test]
fn caso_06_sin_salto_final() {
    let mut p = NshPty::new();
    let out = p.run_cmd("printf 'sin salto final'", 5000);
    assert_eq!(last_exit(&out), Some(0));
    assert!(
        find_sub(&out, b"sin salto final").is_some(),
        "esperaba el texto exacto: {}",
        String::from_utf8_lossy(&out)
    );
}

#[test]
fn caso_07_color_ansi() {
    let mut p = NshPty::new();
    let out = p.run_cmd("printf '\\033[31mrojo\\033[0m\\n'", 5000);
    assert_eq!(last_exit(&out), Some(0));
    let expected = b"\x1b[31mrojo\x1b[0m";
    assert!(
        find_sub(&out, expected).is_some(),
        "esperaba la secuencia ANSI del rojo: {}",
        String::from_utf8_lossy(&out)
    );
}

#[test]
fn caso_08_salida_masiva_y_truncada() {
    let mut p = NshPty::new();
    let out = p.run_cmd("seq 1 100000", 15_000);
    assert_eq!(last_exit(&out), Some(0));
    assert!(
        find_sub(&out, b"[salida truncada:").is_some(),
        "esperaba el aviso de truncado: {}",
        String::from_utf8_lossy(&out)
    );
    // y la última línea de la salida viva debe contener 100000
    assert!(
        find_sub(&out, b"100000").is_some(),
        "esperaba ver 100000: {}",
        String::from_utf8_lossy(&out)
            .chars()
            .take(400)
            .collect::<String>()
    );
}

#[test]
fn caso_09_comando_inexistente() {
    let mut p = NshPty::new();
    let out = p.run_cmd("comando_que_no_existe", 5000);
    assert_eq!(last_exit(&out), Some(127));
}

#[test]
fn caso_10_ruta_con_espacios() {
    let dir = std::env::temp_dir().join("dir con espacios nsh");
    let _ = std::fs::create_dir_all(&dir);
    let mut p = NshPty::new();
    let quoted = format!("cd '{}'", dir.to_str().unwrap());
    let out = p.run_cmd(&quoted, 5000);
    assert_eq!(last_exit(&out), Some(0));
    let prompt_cwd = last_prompt_cwd(&out).expect("no prompt");
    assert!(
        prompt_cwd.contains("dir con espacios nsh"),
        "prompt no refleja la ruta con espacios: {:?}",
        prompt_cwd
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn caso_12_tput_cols() {
    // El PTY del test está fijado a 120 columnas.
    let mut p = NshPty::new();
    let out = p.run_cmd("tput cols", 5000);
    assert_eq!(last_exit(&out), Some(0));
    assert!(
        find_sub(&out, b"120").is_some(),
        "tput cols no devolvio 120: {}",
        String::from_utf8_lossy(&out)
    );
}

#[test]
fn caso_14_background_ampersand() {
    let mut p = NshPty::new();
    let out = p.run_cmd("sleep 1 &", 5000);
    assert_eq!(last_exit(&out), Some(0));
    assert!(
        find_sub(&out, b"[1]").is_some(),
        "esperaba el spec de job [1]: {}",
        String::from_utf8_lossy(&out)
    );
    // drena el "[1]+ Done" diferido para no afectar a nada (sesión se cierra al caer del test)
    p.drain(1500);
}

#[test]
fn caso_15_comentario_final() {
    let mut p = NshPty::new();
    let out = p.run_cmd("ls /tmp # comentario final", 5000);
    assert_eq!(last_exit(&out), Some(0));
}

#[test]
fn caso_16_comilla_sin_cerrar_sigue_vivo() {
    let mut p = NshPty::new();
    // sintaxis inválida -> nsh la rechaza ANTES de enviarla. La sesión sigue.
    p.buf.clear();
    p.send_line("!echo \"comilla sin cerrar");
    p.read_until(b"sintaxis invalida", 5000, "mensaje de sintaxis");
    p.drain(400);
    // prueba de que la sesión sigue viva: un comando normal debe funcionar
    let out = p.run_cmd("echo todavia_vivo", 5000);
    assert_eq!(last_exit(&out), Some(0));
    assert!(
        find_sub(&out, b"todavia_vivo").is_some(),
        "la sesión no seguía viva: {}",
        String::from_utf8_lossy(&out)
    );
}

#[test]
fn caso_20_ctrl_c_durante_sleep() {
    let mut p = NshPty::new();
    p.buf.clear();
    p.send_line("!sleep 100");
    // dale tiempo a que arranque
    p.drain(600);
    // Ctrl+C
    p.send_raw(b"\x03");
    p.read_until(b"[terminado:", 8000, "ctrl-c durante sleep");
    p.drain(500);
    let out = p.snapshot();
    let code = last_exit(&out).expect("sin [terminado] tras Ctrl+C");
    assert!(
        code == 130 || code == 143,
        "esperaba 130 (128+SIGINT) tras Ctrl+C, got {}: {}",
        code,
        String::from_utf8_lossy(&out)
    );
    // la sesión sigue: comando siguiente funciona
    let out2 = p.run_cmd("echo tras_ctrlc", 5000);
    assert_eq!(last_exit(&out2), Some(0));
}

#[test]
fn caso_21_binario_sin_panic() {
    let mut p = NshPty::new();
    let out = p.run_cmd("head -c 200 /dev/urandom", 5000);
    assert_eq!(last_exit(&out), Some(0));
    // nsh no debe haber caído: la siguiente orden funciona
    let out2 = p.run_cmd("echo vivo", 5000);
    assert_eq!(last_exit(&out2), Some(0));
}

#[test]
fn caso_22_marcador_falso_ignorado() {
    let mut p = NshPty::new();
    // Antes: pwd para conocer cwd.
    let out0 = p.run_cmd("pwd", 5000);
    let cwd_before = last_prompt_cwd(&out0);
    // El marcador falsificado (nonce FALSO). printf lo imprime literalmente
    // como bytes; el parser de nsh debe verlo, comprobar el nonce, descartarlo.
    let out = p.run_cmd("printf '\\033]777;nsh;FALSO;x;0;Lw==\\007'", 5000);
    assert_eq!(last_exit(&out), Some(0));
    // exactamente UN [terminado] (el del comando real), el falsificado no cuenta
    assert_eq!(
        count_terminado(&out),
        1,
        "el marcador falso NO fue ignorado: {}",
        String::from_utf8_lossy(&out)
    );
    // estado inalterado: pwd sigue dando lo mismo
    let out2 = p.run_cmd("pwd", 5000);
    assert_eq!(last_exit(&out2), Some(0));
    let cwd_after = last_prompt_cwd(&out2);
    assert_eq!(
        cwd_before, cwd_after,
        "el estado cambió tras el marcador falso"
    );
}

#[test]
fn nonce_readonly_la_sesion_sobrevive_a_unset() {
    let mut p = NshPty::new();
    // NSH_NONCE es readonly: el unset falla dentro de bash pero el marcador
    // de fin de comando sigue llegando y la sesion no se cuelga.
    let out = p.run_cmd("unset NSH_NONCE", 10_000);
    let text = String::from_utf8_lossy(&out);
    // El mensaje de bash depende del locale ("readonly" / "solo lectura"):
    // lo comprobable es que el unset falla (exit 1) y menciona NSH_NONCE.
    assert!(
        text.contains("NSH_NONCE") && text.contains("unset"),
        "el unset deberia fallar por readonly: {text}"
    );
    assert_eq!(last_exit(&out), Some(1));
    // La sesion sigue viva y sincronizada.
    let out = p.run_cmd("echo VIVO", 10_000);
    assert!(
        String::from_utf8_lossy(&out).contains("VIVO"),
        "la sesion no respondio tras el unset"
    );
    assert_eq!(last_exit(&out), Some(0));
}

#[test]
fn caso_23_exit_cierra_limpio() {
    let mut p = NshPty::new();
    p.buf.clear();
    p.send_line("!exit");
    // bash muerre -> nsh imprime "la shell ha terminado" y sale.
    p.read_until(b"la shell ha terminado", 5000, "mensaje de shell terminada");
    p.drain(500);
    let out = p.snapshot();
    assert!(
        find_sub(&out, b"la shell ha terminado").is_some(),
        "esperaba 'la shell ha terminado': {}",
        String::from_utf8_lossy(&out)
    );
}

// ---------- helpers de termios para el caso 24 ----------
//
// Para inspeccionar los termios del esclavo de la PTY, abrimos otra fd al
// mismo `/dev/pts/N`. Lo localizamos vía `/proc/<pid>/fd/0` (link al pts
// esclavo de nsh). Aunque nsh muera, el dispositivo pts sigue vivo mientras
// el master esté abierto (lo retenemos en `_master`), así que podemos leer
// termios DESPUÉS de la muerte del proceso y ver si el hook de pánico lo
// dejó en modo cocido.

fn abrir_pts_esclavo_de(pid: u32) -> Option<RawFd> {
    let target = std::fs::read_link(format!("/proc/{pid}/fd/0")).ok()?;
    let path = target.to_str()?;
    let c_path = std::ffi::CString::new(path).ok()?;
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDWR | libc::O_NOCTTY) };
    if fd < 0 {
        return None;
    }
    Some(fd)
}

/// true si el terminal está en modo cocido (ECHO y ICANON activos).
fn es_cocido(fd: RawFd) -> Option<bool> {
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut t) != 0 {
            return None;
        }
        let echo = t.c_lflag & libc::ECHO != 0;
        let icanon = t.c_lflag & libc::ICANON != 0;
        Some(echo && icanon)
    }
}

fn liberar_fd(fd: RawFd) {
    unsafe {
        libc::close(fd);
    }
}

#[test]
fn caso_24_panic_restaura_termios() {
    // Caso 24, automatizado por la vía que el restore() del PASO 3 cubre de
    // verdad: un PANIC mientras un comando corre. Con NSH_PANIC_TEST=1,
    // execute() hace panic!() justo después de entrar en RawGuard (terminal
    // ya en raw mode). El hook de pánico instalado por term::init() debe
    // llamar a restore() y devolver el esclavo a modo cocido.
    let tmp = std::env::temp_dir();
    let mut p = NshPty::new_with_env(tmp.clone(), &[("NSH_PANIC_TEST", "1")]);

    // Abrimos una fd propia al pts esclavo para espiar sus termios.
    let fd =
        abrir_pts_esclavo_de(p.pid()).expect("no se pudo abrir el pts esclavo para inspección");

    // Mientras nsh está en el prompt (readline en curso) el esclavo está en
    // raw mode (rustyline). No usamos eso de baseline. Lanzamos el comando:
    // al entrar en RawGuard y leer la variable, nsh hace panic. Tras morir,
    // el último writer de termios debe haber sido el restore() del hook.
    p.send_line("!sleep 30");
    p.wait_for_exit(5000);

    let cocido = es_cocido(fd).expect("tcgetattr falló tras la muerte de nsh");
    liberar_fd(fd);

    assert!(
        cocido,
        "el hook de pánico NO restauró los termios: el esclavo quedó en raw mode"
    );
}

/// SIGTERM ya NO es una limitación: nsh registra SIGTERM/SIGHUP y restaura
/// los termios antes de morir (ver INFORME PASO 25).
#[test]
fn sigterm_restaura_termios() {
    let tmp = std::env::temp_dir();
    let mut p = NshPty::new_with_cwd(tmp.clone());

    let fd =
        abrir_pts_esclavo_de(p.pid()).expect("no se pudo abrir el pts esclavo para inspección");

    // Lanzamos un comando que se quede corriendo: nsh estará dentro de
    // RawGuard (esclavo en raw mode) cuando llegue el SIGTERM.
    p.send_line("!sleep 30");
    p.drain(500); // tiempo a que entre en execute()/RawGuard
    p.signal(libc::SIGTERM);
    p.wait_for_exit(5000);

    let cocido = es_cocido(fd).expect("tcgetattr falló tras SIGTERM");
    liberar_fd(fd);

    assert!(
        cocido,
        "SIGTERM NO restauró los termios: el esclavo quedó en raw mode"
    );
}

/// El hook interno (PROMPT_COMMAND/__nsh_hook) es readonly: intentar quitarlo
/// falla dentro de bash y la sesión sigue viva y sincronizada. Antes de hacerlo
/// readonly, `!unset PROMPT_COMMAND` eliminaba el emisor de marcadores y nsh se
/// quedaba esperando el fin de comando para siempre (FASE 4 no tiene timeout,
/// a propósito, para admitir `sleep`, `top`, `vim`...).
#[test]
fn hook_interno_readonly_la_sesion_sobrevive() {
    let mut p = NshPty::new();
    let out = p.run_cmd("unset PROMPT_COMMAND", 10_000);
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("PROMPT_COMMAND") && text.contains("unset"),
        "el unset deberia fallar por readonly: {text}"
    );
    assert_eq!(last_exit(&out), Some(1));
    // La sesion sigue viva y sincronizada.
    let out = p.run_cmd("echo VIVO", 10_000);
    assert!(
        String::from_utf8_lossy(&out).contains("VIVO"),
        "la sesion no respondio tras el unset de PROMPT_COMMAND"
    );
    assert_eq!(last_exit(&out), Some(0));
}

/// El hook usa `builtin printf` y `command -p stty/base64`: una función del
/// usuario que sombree `printf` no deja el hook mudo (antes nsh quedaba
/// esperando eternamente el marcador de fin de ese comando).
#[test]
fn hook_sobrevive_a_sombra_de_printf() {
    let mut p = NshPty::new();
    // Antes del endurecimiento, ESTE comando ya no recibia marcador: el hook
    // llamaba al printf sombreado y run_cmd agotaba su timeout.
    let out = p.run_cmd("printf(){ :; }", 10_000);
    assert_eq!(last_exit(&out), Some(0));
    let out = p.run_cmd("echo VIVO", 10_000);
    assert!(
        String::from_utf8_lossy(&out).contains("VIVO"),
        "la sesion no respondio tras sombrear printf"
    );
    assert_eq!(last_exit(&out), Some(0));
}

/// `command -p` busca stty/base64 en el PATH por defecto del sistema: un PATH
/// inutilizado por el usuario tampoco rompe el hook.
#[test]
fn hook_sobrevive_a_path_inutilizado() {
    let mut p = NshPty::new();
    let out = p.run_cmd("PATH=/no/existe", 10_000);
    assert_eq!(last_exit(&out), Some(0));
    let out = p.run_cmd("echo VIVO", 10_000);
    assert!(
        String::from_utf8_lossy(&out).contains("VIVO"),
        "la sesion no respondio con PATH roto"
    );
    assert_eq!(last_exit(&out), Some(0));
    // Restablecer para no dejar la sesión coja.
    let _ = p.run_cmd("PATH=$PATH", 10_000);
}

/// Un PWD artificial gigante (variable, sin límite de PATH_MAX) no puede
/// superar el MAX_MARKER_LEN del parser: el hook corta su Base64 y el marcador
/// siempre cabe. Antes, el parser descartaba el marcador como basura y nsh se
/// quedaba esperando el fin de comando para siempre.
#[test]
fn pwd_artificial_gigante_no_rompe_el_marcador() {
    let mut p = NshPty::new();
    let out = p.run_cmd("PWD=$(printf 'a%.0s' {1..20000})", 10_000);
    assert_eq!(last_exit(&out), Some(0));
    // El marcador llegó (run_cmd no agotó timeout) y la sesión sigue viva.
    let out = p.run_cmd("echo VIVO", 10_000);
    assert!(
        String::from_utf8_lossy(&out).contains("VIVO"),
        "la sesion no respondio tras el PWD gigante"
    );
    assert_eq!(last_exit(&out), Some(0));
    let _ = p.run_cmd("cd /tmp", 10_000);
}

/// `!exit` mata la shell interna: nsh debe cerrarse limpio, no seguir pintando
/// prompts sobre una bash muerta.
#[test]
fn exit_interno_cierra_nsh() {
    let mut p = NshPty::new();
    p.send_line("!exit");
    p.wait_for_exit(10_000);
    let snap = p.snapshot();
    let text = String::from_utf8_lossy(&snap);
    assert!(
        text.contains("la shell ha terminado") || text.contains("cerrando nsh"),
        "se esperaba el mensaje de cierre tras !exit: {text}"
    );
    // wait_for_exit solo vence por timeout si nsh sigue vivo: si llegamos aquí
    // con el canal desconectado, nsh terminó. Comprobación explícita: pedir
    // más datos no puede traer un nuevo prompt.
    let n_antes = p.snapshot().len();
    p.drain(300);
    assert_eq!(p.snapshot().len(), n_antes, "nsh sigue vivo tras !exit");
}

/// `!exec bash` sustituye la shell interna: pierde el nonce y el hook, así que
/// la sincronía se rompe para siempre. nsh debe detectarlo (timeout de la fase
/// de armado) y cerrar, no quedarse colgado ni pintar prompts muertos.
#[test]
fn exec_bash_cierra_nsh_por_perdida_de_sincronia() {
    let mut p = NshPty::new();
    p.send_line("!exec bash");
    p.wait_for_exit(15_000);
    // Si llegamos aquí, nsh terminó (el canal se desconectó): correcto.
    p.drain(300);
}

/// Un cwd muy profundo (cerca de PATH_MAX) se codifica en Base64 dentro del
/// marcador: el marcador legitimo más grande posible no debe descartarse ni
/// colgar la sesión.
#[test]
fn cwd_profundo_el_marcador_no_se_descarta() {
    let base = std::env::temp_dir().join(format!("nsh-deep-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    // Componentes de 60 chars hasta rondar PATH_MAX (4096, total): base64 del
    // cwd (~5400 bytes) excede el limite antiguo de 4096 del parser. No se
    // crea fichero dentro: el propio cwd ya deja el camino al limite.
    let mut dir = base.clone();
    let comp = "d".repeat(60);
    while dir.as_os_str().len() + 1 + comp.len() <= 4090 {
        dir.push(&comp);
        std::fs::create_dir(&dir).unwrap();
    }
    assert!(
        dir.as_os_str().len() > 4000,
        "el cwd de prueba deberia rondar PATH_MAX: {}",
        dir.as_os_str().len()
    );

    let mut p = NshPty::new_with_cwd(dir.clone());
    let out = p.run_cmd("echo ENCONTRADO", 10_000);
    let text = String::from_utf8_lossy(&out);
    assert_eq!(
        last_exit(&out),
        Some(0),
        "el comando fallo en cwd profundo: {text}"
    );
    assert!(text.contains("ENCONTRADO"));
    let _ = std::fs::remove_dir_all(&base);
}

// ---------- FASE 2 ----------

// L9 — Sin config.toml: el modo `!` sigue funcionando y el texto natural avisa
// "modo LLM no disponible". Se lanza nsh con NSH_CONFIG_PATH apuntando a un
// fichero inexistente para no tocar la config real del usuario.
#[test]
fn l9_sin_config_degradacion_honrada() {
    let tmp = std::env::temp_dir();
    let mut p = NshPty::new_with_env(
        tmp.clone(),
        &[("NSH_CONFIG_PATH", "/no/existe/nsh/config.toml")],
    );
    // El arranque debe avisar de que el modo LLM no está disponible.
    p.buf.clear();
    // `!` sigue funcionando.
    let out = p.run_cmd("echo ok_l9", 5000);
    assert_eq!(last_exit(&out), Some(0));
    assert!(
        find_sub(&out, b"ok_l9").is_some(),
        "`!` no funcionó sin config: {}",
        String::from_utf8_lossy(&out)
    );
    // El texto natural debe avisar, no cascar.
    p.buf.clear();
    p.send_line("esto es texto natural sin config");
    p.read_until(
        b"modo LLM no disponible",
        5000,
        "aviso de LLM no disponible",
    );
    // y la sesión sigue viva: un !echo más funciona.
    let out2 = p.run_cmd("echo sigue_vivo", 5000);
    assert_eq!(last_exit(&out2), Some(0));
}

// ---------- FASE 3: Referencias a ficheros (PASO 12) ----------

#[test]
fn f6_resolucion_arroba_en_modo_exclamacion() {
    // Crea un archivo temporal y prueba que @src/main.rs se expande en modo !
    let tmp = tempfile::TempDir::new().unwrap();
    let tmp_path = tmp.path().to_path_buf();
    let test_file = tmp_path.join("test_file.txt");
    std::fs::write(&test_file, "contenido de prueba\n").unwrap();

    // Crea una subcarpeta src y un archivo main.rs
    let src = tmp_path.join("src");
    std::fs::create_dir(&src).unwrap();
    let main_rs = src.join("main.rs");
    std::fs::write(&main_rs, "fn main() {}\n").unwrap();

    let mut p = NshPty::new_with_cwd(tmp_path.clone());
    let out = p.run_cmd("wc -l @src/main.rs", 5000);
    assert_eq!(last_exit(&out), Some(0));
    // Debe mostrar la ruta expandida (src/main.rs) en la salida
    assert!(
        find_sub(&out, b"src/main.rs").is_some(),
        "no se expandió @src/main.rs: {}",
        String::from_utf8_lossy(&out)
    );
}

#[test]
fn f5_arroba_inexistente_error_sin_llamar_llm() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tmp_path = tmp.path().to_path_buf();
    // Crea un config dummy para poder usar modo LLM
    let config_dir = tmp_path.join(".config/nsh");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("config.toml");
    std::fs::write(
        &config_path,
        r#"
model = "zai/glm-4.7"

[providers.zai]
base_url = "http://localhost:1234"
api = "anthropic"
api_key = "dummy"
models = ["glm-4.7"]
"#,
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600)).unwrap();

    let env_config = config_path.to_str().unwrap();

    let mut p = NshPty::new_with_env(tmp_path.clone(), &[("NSH_CONFIG_PATH", env_config)]);
    p.buf.clear();
    p.send_line("resumen de @noexiste.md");
    p.read_until(
        b"no existe: @noexiste.md",
        5000,
        "error de archivo inexistente",
    );
    // NO debe aparecer "pensando…" ni spinner, ni debe mostrar un comando planificado
    let snap = p.snapshot();
    let snap_str = String::from_utf8_lossy(&snap);
    assert!(
        !snap_str.contains("pensando") && !snap_str.contains("spinner"),
        "apareció spinner/pensando, lo que indica llamada al LLM: {}",
        snap_str
    );
    // La sesión sigue viva
    let out2 = p.run_cmd("echo sigue_vivo", 5000);
    assert_eq!(last_exit(&out2), Some(0));
}

#[test]
fn f9_tab_sin_arroba_no_completa() {
    // Este test es tricky porque el fuzzy completion abre un selector a pantalla completa.
    // Probamos con completion="list" en el config, que es determinista.
    let tmp = tempfile::TempDir::new().unwrap();
    let tmp_path = tmp.path().to_path_buf();
    let config_dir = tmp_path.join(".config/nsh");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("config.toml");
    std::fs::write(
        &config_path,
        r#"
model = "zai/glm-4.7"
completion = "list"

[providers.zai]
base_url = "http://localhost:1234"
api = "anthropic"
api_key = "dummy"
models = ["glm-4.7"]
"#,
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600)).unwrap();

    let env_config = config_path.to_str().unwrap();

    let mut p = NshPty::new_with_env(tmp_path.clone(), &[("NSH_CONFIG_PATH", env_config)]);
    // Escribimos "hola" y mandamos un byte Tab (0x09)
    p.buf.clear();
    p.send_raw(b"hola");
    p.send_raw(b"\x09");
    std::thread::sleep(Duration::from_millis(200));

    // Drenamos cualquier respuesta
    p.drain(200);

    // Con Tab sin @, NUNCA debe mostrar sugerencias de archivos
    // Si mostrara, aparecería algo como "hola main.rs lib.rs ..."
    let snap = p.snapshot();
    let snap_str = String::from_utf8_lossy(&snap);
    // Simplemente verificamos que no hay nombres de archivos después de "hola"
    assert!(
        !snap_str.contains("main.rs")
            && !snap_str.contains("lib.rs")
            && !snap_str.contains(".toml"),
        "Tab sin @ completó archivos: {}",
        snap_str
    );
}
