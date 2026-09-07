use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use std::path::PathBuf;

const BEL: u8 = 0x07;
const PREFIX: &[u8] = b"\x1b]777;nsh;";
/// Si algo empieza como un marcador pero no cierra en este numero de bytes,
/// se considera basura y se emite como salida normal. Evita bloqueos.
/// Debe superar el peor caso legitimo: PATH_MAX (4096) de cwd + base64 (x4/3
/// = ~5462) + nonce + id + exit + prefijo. Con 4096 un cwd suficientemente
/// profundo hacia que el parser descartara el marcador como basura y nsh
/// esperara eternamente un fin de comando que ya habia llegado.
const MAX_MARKER_LEN: usize = 8192;

#[derive(Debug)]
pub enum Event {
    Output(Vec<u8>),
    Finished {
        id: String,
        exit_code: i32,
        cwd: PathBuf,
    },
}

pub struct Parser {
    buf: Vec<u8>,
    nonce: String,
}

impl Parser {
    pub fn new(nonce: &str) -> Self {
        Parser {
            buf: Vec::new(),
            nonce: nonce.to_string(),
        }
    }

    pub fn push(&mut self, data: &[u8]) -> Vec<Event> {
        self.buf.extend_from_slice(data);
        let mut events = Vec::new();

        loop {
            match find(&self.buf, PREFIX) {
                Some(start) => {
                    // Todo lo anterior al marcador es salida normal.
                    if start > 0 {
                        let out: Vec<u8> = self.buf.drain(..start).collect();
                        events.push(Event::Output(out));
                    }
                    // Ahora self.buf empieza por PREFIX. Buscamos el BEL de cierre.
                    match self.buf.iter().position(|&b| b == BEL) {
                        Some(end) => {
                            let marker: Vec<u8> = self.buf.drain(..=end).collect();
                            if let Some(ev) = self.parse_marker(&marker) {
                                events.push(ev);
                            }
                            // Si parse_marker devuelve None (nonce malo), se descarta.
                        }
                        None => {
                            if self.buf.len() > MAX_MARKER_LEN {
                                // No es un marcador nuestro: emitelo y sigue.
                                let out: Vec<u8> = self.buf.drain(..).collect();
                                events.push(Event::Output(out));
                                continue;
                            }
                            break; // marcador incompleto, esperamos mas bytes
                        }
                    }
                }
                None => {
                    // No hay marcador completo. Retenemos SOLO la cola que
                    // todavia podria ser el principio de PREFIX (max 10 bytes).
                    let keep = partial_prefix_len(&self.buf);
                    let emit = self.buf.len() - keep;
                    if emit > 0 {
                        let out: Vec<u8> = self.buf.drain(..emit).collect();
                        events.push(Event::Output(out));
                    }
                    break;
                }
            }
        }

        events
    }

    fn parse_marker(&self, marker: &[u8]) -> Option<Event> {
        // marker = PREFIX + "NONCE;ID;EXIT;CWD_B64" + BEL
        let body = marker.get(PREFIX.len()..marker.len() - 1)?;
        let text = std::str::from_utf8(body).ok()?;

        let mut parts = text.splitn(4, ';');
        let nonce = parts.next()?;
        let id = parts.next()?;
        let exit = parts.next()?;
        let cwd_b64 = parts.next()?;

        if nonce != self.nonce {
            return None; // marcador falsificado
        }

        let exit_code: i32 = exit.parse().ok()?;
        let cwd_bytes = B64.decode(cwd_b64).ok()?;
        let cwd = PathBuf::from(String::from_utf8_lossy(&cwd_bytes).into_owned());

        Some(Event::Finished {
            id: id.to_string(),
            exit_code,
            cwd,
        })
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Longitud de la cola de `buf` que es un prefijo propio de PREFIX.
/// Devuelve 0 si la cola no puede ser el principio de un marcador.
fn partial_prefix_len(buf: &[u8]) -> usize {
    let max = PREFIX.len().min(buf.len());
    // OJO: rango INCLUSIVO. Con `1..max` no se comprueba n == max, que es justo
    // el caso del test `marcador_partido_en_tres_lecturas` (buffer de 5 bytes
    // "\x1b]777", que es PREFIX[..5]): se emitirian esos bytes en vez de
    // retenerlos. Es seguro porque esta rama solo se alcanza cuando NO hay un
    // PREFIX completo, asi que n == PREFIX.len() nunca puede acertar.
    // (Bug del plan original —rango excluyente— detectado en la implementación
    // y corregido ya en el plan canónico; este es el texto canónico.)
    for n in (1..=max).rev() {
        if &buf[buf.len() - n..] == &PREFIX[..n] {
            return n;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker(nonce: &str, id: &str, exit: i32, cwd_b64: &str) -> Vec<u8> {
        format!("\x1b]777;nsh;{nonce};{id};{exit};{cwd_b64}\x07").into_bytes()
    }

    #[test]
    fn salida_simple() {
        let mut p = Parser::new("n1");
        let ev = p.push(b"hola\r\n");
        assert!(matches!(&ev[0], Event::Output(b) if b == b"hola\r\n"));
    }

    #[test]
    fn marcador_completo() {
        let mut p = Parser::new("n1");
        let mut data = b"salida".to_vec();
        data.extend(marker("n1", "c1", 7, "L3RtcA=="));
        let ev = p.push(&data);
        assert_eq!(ev.len(), 2);
        match &ev[1] {
            Event::Finished { id, exit_code, cwd } => {
                assert_eq!(id, "c1");
                assert_eq!(*exit_code, 7);
                assert_eq!(cwd.to_str().unwrap(), "/tmp");
            }
            _ => panic!("esperaba Finished"),
        }
    }

    #[test]
    fn marcador_partido_en_tres_lecturas() {
        let mut p = Parser::new("n1");
        let m = marker("n1", "c9", 0, "L3RtcA==");
        let a = &m[..5];
        let b = &m[5..12];
        let c = &m[12..];
        assert!(p.push(a).is_empty());
        assert!(p.push(b).is_empty());
        let ev = p.push(c);
        assert!(matches!(&ev[0], Event::Finished { id, .. } if id == "c9"));
    }

    #[test]
    fn marcador_con_cwd_largo_no_se_descarta() {
        // PATH_MAX (4096) de cwd + base64 (x4/3): el marcador legitimo mas
        // grande posible debe parsear, no emitirse como salida.
        let cwd_largo = "/".to_string() + &"a".repeat(4095);
        let b64 = B64.encode(cwd_largo.as_bytes());
        let m = marker("n1", "c1", 0, &b64);
        assert!(m.len() > 4096, "el test deja de probar nada si el marcador cabe en el limite antiguo");

        let mut p = Parser::new("n1");
        let ev = p.push(&m);
        match &ev[0] {
            Event::Finished { cwd, .. } => assert_eq!(cwd, &PathBuf::from(cwd_largo)),
            _ => panic!("el marcador con cwd largo se descarto: {:?}", ev.len()),
        }
    }

    #[test]
    fn nonce_falso_se_descarta() {
        let mut p = Parser::new("bueno");
        let ev = p.push(&marker("malo", "x", 0, "Lw=="));
        assert!(ev.iter().all(|e| !matches!(e, Event::Finished { .. })));
    }

    #[test]
    fn ansi_no_se_retiene() {
        // Un ESC que NO es principio de marcador debe emitirse ya,
        // si no las TUI se ven a saltos.
        let mut p = Parser::new("n1");
        let ev = p.push(b"\x1b[31mrojo\x1b[0m");
        let total: Vec<u8> = ev
            .into_iter()
            .flat_map(|e| match e {
                Event::Output(b) => b,
                _ => vec![],
            })
            .collect();
        assert_eq!(total, b"\x1b[31mrojo\x1b[0m");
    }

    #[test]
    fn cola_que_si_es_prefijo_se_retiene() {
        let mut p = Parser::new("n1");
        let ev = p.push(b"abc\x1b]777;");
        let total: Vec<u8> = ev
            .into_iter()
            .flat_map(|e| match e {
                Event::Output(b) => b,
                _ => vec![],
            })
            .collect();
        assert_eq!(total, b"abc"); // el resto queda retenido
    }
}
