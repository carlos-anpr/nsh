use std::sync::OnceLock;

/// Envoltorio para poder guardar un termios en una estatica.
struct TermiosBox(libc::termios);
unsafe impl Send for TermiosBox {}
unsafe impl Sync for TermiosBox {}

static ORIG: OnceLock<TermiosBox> = OnceLock::new();

/// Guarda el estado original del terminal e instala un hook de panico que lo
/// restaura. Llamar UNA VEZ al arrancar, antes de nada.
pub fn init() {
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut t) == 0 {
            let _ = ORIG.set(TermiosBox(t));
        }
    }
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        prev(info);
    }));
}

/// Devuelve el terminal a modo cocido. Idempotente.
pub fn restore() {
    if let Some(o) = ORIG.get() {
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &o.0);
        }
    }
}

/// Guard RAII: mientras vive, el terminal real esta en raw mode.
pub struct RawGuard;

impl RawGuard {
    pub fn enter() -> RawGuard {
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut t) == 0 {
                libc::cfmakeraw(&mut t);
                // VMIN=1, VTIME=0: read() devuelve en cuanto hay 1 byte.
                t.c_cc[libc::VMIN] = 1;
                t.c_cc[libc::VTIME] = 0;
                libc::tcsetattr(0, libc::TCSANOW, &t);
            }
        }
        RawGuard
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        restore();
    }
}

/// Tamano actual del terminal real. (filas, columnas). Por defecto 24x80.
pub fn window_size() -> (u16, u16) {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(0, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_row > 0 && ws.ws_col > 0 {
            (ws.ws_row, ws.ws_col)
        } else {
            (24, 80)
        }
    }
}
