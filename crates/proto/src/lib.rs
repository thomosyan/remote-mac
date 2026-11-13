//! Протокол между хостом (macOS) и клиентом (Windows).
//!
//! Формат одного сообщения в TCP-потоке:
//!   [длина payload: u32 LE][тип: u8][payload]
//!
//! Только стандартная библиотека, без внешних зависимостей.

use std::io::{self, Read, Write};

pub mod annexb;
pub use annexb::AuSplitter;

/// Версия протокола. Хост и клиент должны совпадать.
pub const VERSION: u8 = 1;

/// Максимальный размер одного сообщения (защита от мусора в сети).
pub const MAX_PAYLOAD: usize = 32 * 1024 * 1024;

const HEADER_LEN: usize = 5;

const T_HELLO: u8 = 1;
const T_WELCOME: u8 = 2;
const T_ERROR: u8 = 3;
const T_VIDEO: u8 = 4;
const T_MOUSE_MOVE: u8 = 5;
const T_MOUSE_BUTTON: u8 = 6;
const T_SCROLL: u8 = 7;
const T_KEY: u8 = 8;
const T_PING: u8 = 9;
const T_PONG: u8 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Button {
    Left = 0,
    Right = 1,
    Middle = 2,
}

impl Button {
    pub fn index(self) -> usize {
        self as usize
    }

    fn from_u8(v: u8) -> Option<Button> {
        match v {
            0 => Some(Button::Left),
            1 => Some(Button::Right),
            2 => Some(Button::Middle),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// клиент -> хост: первое сообщение, версия и токен доступа
    Hello { version: u8, token: Vec<u8> },
    /// хост -> клиент: токен принят, сейчас пойдёт видео
    Welcome,
    /// хост -> клиент: отказ с текстом причины
    Error(String),
    /// хост -> клиент: один access unit H.264 в формате Annex B
    Video(Vec<u8>),
    /// клиент -> хост: абсолютная позиция мыши, нормированная 0.0..=1.0
    MouseMove { x: f32, y: f32 },
    MouseButton { button: Button, down: bool },
    /// прокрутка в пикселях (dy > 0 — вверх)
    Scroll { dx: i16, dy: i16 },
    /// код клавиши — виртуальный keycode macOS (kVK_*)
    Key { code: u16, down: bool },
    /// клиент -> хост, хост отвечает Pong с тем же числом
    Ping(u64),
    Pong(u64),
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

fn need(p: &[u8], n: usize) -> io::Result<()> {
    if p.len() < n {
        Err(invalid("payload too short"))
    } else {
        Ok(())
    }
}

impl Message {
    fn type_id(&self) -> u8 {
        match self {
            Message::Hello { .. } => T_HELLO,
            Message::Welcome => T_WELCOME,
            Message::Error(_) => T_ERROR,
            Message::Video(_) => T_VIDEO,
            Message::MouseMove { .. } => T_MOUSE_MOVE,
            Message::MouseButton { .. } => T_MOUSE_BUTTON,
            Message::Scroll { .. } => T_SCROLL,
            Message::Key { .. } => T_KEY,
            Message::Ping(_) => T_PING,
            Message::Pong(_) => T_PONG,
        }
    }

    fn payload_hint(&self) -> usize {
        match self {
            Message::Video(d) => d.len(),
            Message::Hello { token, .. } => token.len() + 1,
            Message::Error(s) => s.len(),
            _ => 16,
        }
    }

    /// Собирает сообщение целиком в один буфер: одно сетевое обращение на сообщение.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.payload_hint());
        out.extend_from_slice(&[0u8; 4]);
        out.push(self.type_id());
        match self {
            Message::Hello { version, token } => {
                out.push(*version);
                out.extend_from_slice(token);
            }
            Message::Welcome => {}
            Message::Error(text) => out.extend_from_slice(text.as_bytes()),
            Message::Video(data) => out.extend_from_slice(data),
            Message::MouseMove { x, y } => {
                out.extend_from_slice(&x.to_le_bytes());
                out.extend_from_slice(&y.to_le_bytes());
            }
            Message::MouseButton { button, down } => {
                out.push(*button as u8);
                out.push(*down as u8);
            }
            Message::Scroll { dx, dy } => {
                out.extend_from_slice(&dx.to_le_bytes());
                out.extend_from_slice(&dy.to_le_bytes());
            }
            Message::Key { code, down } => {
                out.extend_from_slice(&code.to_le_bytes());
                out.push(*down as u8);
            }
            Message::Ping(v) | Message::Pong(v) => out.extend_from_slice(&v.to_le_bytes()),
        }
        let len = (out.len() - HEADER_LEN) as u32;
        out[0..4].copy_from_slice(&len.to_le_bytes());
        out
    }

    fn decode(t: u8, p: &[u8]) -> io::Result<Message> {
        let msg = match t {
            T_HELLO => {
                need(p, 1)?;
                Message::Hello {
                    version: p[0],
                    token: p[1..].to_vec(),
                }
            }
            T_WELCOME => Message::Welcome,
            T_ERROR => Message::Error(String::from_utf8_lossy(p).into_owned()),
            T_VIDEO => Message::Video(p.to_vec()),
            T_MOUSE_MOVE => {
                need(p, 8)?;
                Message::MouseMove {
                    x: f32::from_le_bytes([p[0], p[1], p[2], p[3]]),
                    y: f32::from_le_bytes([p[4], p[5], p[6], p[7]]),
                }
            }
            T_MOUSE_BUTTON => {
                need(p, 2)?;
                let button = Button::from_u8(p[0]).ok_or_else(|| invalid("unknown button"))?;
                Message::MouseButton {
                    button,
                    down: p[1] != 0,
                }
            }
            T_SCROLL => {
                need(p, 4)?;
                Message::Scroll {
                    dx: i16::from_le_bytes([p[0], p[1]]),
                    dy: i16::from_le_bytes([p[2], p[3]]),
                }
            }
            T_KEY => {
                need(p, 3)?;
                Message::Key {
                    code: u16::from_le_bytes([p[0], p[1]]),
                    down: p[2] != 0,
                }
            }
            T_PING | T_PONG => {
                need(p, 8)?;
                let mut b = [0u8; 8];
                b.copy_from_slice(&p[..8]);
                let v = u64::from_le_bytes(b);
                if t == T_PING {
                    Message::Ping(v)
                } else {
                    Message::Pong(v)
                }
            }
            _ => return Err(invalid("unknown message type")),
        };
        Ok(msg)
    }
}

pub fn write_message<W: Write>(w: &mut W, msg: &Message) -> io::Result<()> {
    let buf = msg.encode();
    w.write_all(&buf)?;
    w.flush()
}

pub fn read_message<R: Read>(r: &mut R) -> io::Result<Message> {
    let mut header = [0u8; HEADER_LEN];
    r.read_exact(&mut header)?;
    let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    if len > MAX_PAYLOAD {
        return Err(invalid("payload too large"));
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    Message::decode(header[4], &payload)
}

/// Сравнение токенов без раннего выхода по первому отличию.
pub fn tokens_equal(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn roundtrip(msg: Message) {
        let bytes = msg.encode();
        let mut cur = Cursor::new(bytes);
        let back = read_message(&mut cur).expect("decode");
        assert_eq!(back, msg);
        assert_eq!(cur.position() as usize, cur.get_ref().len());
    }

    #[test]
    fn all_messages_roundtrip() {
        roundtrip(Message::Hello {
            version: VERSION,
            token: b"abc123".to_vec(),
        });
        roundtrip(Message::Welcome);
        roundtrip(Message::Error("неверный токен".to_string()));
        roundtrip(Message::Video(vec![0, 0, 0, 1, 0x65, 1, 2, 3]));
        roundtrip(Message::Video(Vec::new()));
        roundtrip(Message::MouseMove { x: 0.25, y: 0.75 });
        roundtrip(Message::MouseButton {
            button: Button::Middle,
            down: true,
        });
        roundtrip(Message::Scroll { dx: -30, dy: 120 });
        roundtrip(Message::Key {
            code: 126,
            down: false,
        });
        roundtrip(Message::Ping(u64::MAX - 7));
        roundtrip(Message::Pong(42));
    }

    #[test]
    fn several_messages_in_one_stream() {
        let mut buf = Vec::new();
        write_message(&mut buf, &Message::Ping(1)).unwrap();
        write_message(&mut buf, &Message::Video(vec![9; 100_000])).unwrap();
        write_message(&mut buf, &Message::Pong(2)).unwrap();
        let mut cur = Cursor::new(buf);
        assert_eq!(read_message(&mut cur).unwrap(), Message::Ping(1));
        assert_eq!(
            read_message(&mut cur).unwrap(),
            Message::Video(vec![9; 100_000])
        );
        assert_eq!(read_message(&mut cur).unwrap(), Message::Pong(2));
    }

    #[test]
    fn oversize_is_rejected() {
        let mut bytes = ((MAX_PAYLOAD as u32) + 1).to_le_bytes().to_vec();
        bytes.push(T_VIDEO);
        let err = read_message(&mut Cursor::new(bytes)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn truncated_stream_is_eof() {
        let mut bytes = Message::Video(vec![1, 2, 3, 4]).encode();
        bytes.truncate(bytes.len() - 2);
        let err = read_message(&mut Cursor::new(bytes)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn unknown_type_and_short_payload_are_rejected() {
        let mut bytes = 0u32.to_le_bytes().to_vec();
        bytes.push(200);
        assert!(read_message(&mut Cursor::new(bytes)).is_err());

        // MouseMove с коротким payload
        let mut bytes = 3u32.to_le_bytes().to_vec();
        bytes.push(T_MOUSE_MOVE);
        bytes.extend_from_slice(&[1, 2, 3]);
        assert!(read_message(&mut Cursor::new(bytes)).is_err());
    }

    #[test]
    fn token_compare() {
        assert!(tokens_equal(b"secret", b"secret"));
        assert!(!tokens_equal(b"secret", b"secreT"));
        assert!(!tokens_equal(b"secret", b"secret1"));
    }
}
