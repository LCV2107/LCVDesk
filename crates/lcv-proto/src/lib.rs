//! Mensajes del protocolo LCVDesk y su tramado sobre un flujo de bytes (TCP).
//!
//! Trama: `[u32 little-endian: longitud][mensaje serializado con postcard]`.

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

pub const PROTOCOL_VERSION: u32 = 1;
pub const DEFAULT_PORT: u16 = 47070;

/// Límite de seguridad para una trama (un fotograma 4K intra cabe holgadamente).
const MAX_MESSAGE_LEN: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Message {
    /// Visor → host: primer mensaje de la conexión.
    Hello { version: u32, name: String },
    /// Host → visor: el usuario aceptó. Lista de monitores y cuál se transmite al principio.
    Accepted { displays: Vec<DisplayInfo>, current: u32 },
    /// Host → visor: conexión rechazada.
    Rejected { reason: String },
    /// Medición de latencia de ida y vuelta: el receptor responde `Pong` con el mismo valor.
    Ping { t_us: u64 },
    Pong { t_us: u64 },
    VideoFrame(VideoFrame),
    /// Visor → host: el decodificador perdió la sincronía; pide un fotograma IDR.
    RequestKeyframe,
    /// Visor → host: cambiar al monitor indicado (índice dentro de la lista `displays`).
    SelectDisplay { index: u32 },
    /// Host → visor: la lista de monitores cambió (se conectó/desconectó una pantalla, cambió la resolución).
    Displays { displays: Vec<DisplayInfo>, current: u32 },
    // --- Entrada remota (visor → host). Las coordenadas del ratón son absolutas y normalizadas. ---
    /// Posición del ratón en 0..=65535 sobre el monitor que se está viendo.
    MouseMove { x: u16, y: u16 },
    MouseButton { button: MouseButton, down: bool },
    /// Rueda: pasos de rueda (120 = una muesca), vertical y horizontal.
    MouseWheel { dx: i32, dy: i32 },
    /// Tecla por scancode de hardware (independiente de la distribución del teclado).
    Key { scancode: u16, extended: bool, down: bool },
    /// Texto Unicode directo (para distribuciones que el scancode no cubre; opcional).
    Unicode { ch: u32 },
    /// Soltar todas las teclas/botones (al perder el foco o desconectar); evita teclas "pegadas".
    ReleaseAll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplayInfo {
    pub index: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// Posición en el escritorio virtual (para ordenar los monitores en la interfaz).
    pub x: i32,
    pub y: i32,
    pub primary: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoFrame {
    pub seq: u64,
    pub width: u32,
    pub height: u32,
    pub keyframe: bool,
    /// Microsegundos que tardó el host en capturar, convertir y codificar este fotograma.
    pub host_us: u32,
    /// Flujo H.264 Annex-B.
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

pub fn write_message<W: Write>(w: &mut W, msg: &Message) -> io::Result<()> {
    // Cabecera y cuerpo en un solo buffer: con TCP_NODELAY evita enviar la cabecera en un segmento aparte.
    let mut buf = postcard::to_extend(msg, vec![0u8; 4]).map_err(invalid_data)?;
    let len = u32::try_from(buf.len() - 4).map_err(invalid_data)?;
    buf[..4].copy_from_slice(&len.to_le_bytes());
    w.write_all(&buf)?;
    w.flush()
}

pub fn read_message<R: Read>(r: &mut R) -> io::Result<Message> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_MESSAGE_LEN {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("trama demasiado grande: {len} bytes")));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    postcard::from_bytes(&body).map_err(invalid_data)
}

fn invalid_data<E: std::error::Error + Send + Sync + 'static>(e: E) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let msgs = [
            Message::Hello { version: PROTOCOL_VERSION, name: "PC-1".into() },
            Message::Ping { t_us: 123_456 },
            Message::MouseMove { x: 32768, y: 100 },
            Message::Key { scancode: 0x1c, extended: false, down: true },
            Message::Accepted {
                displays: vec![DisplayInfo {
                    index: 0,
                    name: "\\\\.\\DISPLAY1".into(),
                    width: 1920,
                    height: 1080,
                    x: 0,
                    y: 0,
                    primary: true,
                }],
                current: 0,
            },
            Message::VideoFrame(VideoFrame {
                seq: 7,
                width: 1920,
                height: 1080,
                keyframe: true,
                host_us: 4200,
                data: vec![0, 0, 0, 1, 0x67, 0x42],
            }),
        ];
        let mut wire = Vec::new();
        for m in &msgs {
            write_message(&mut wire, m).unwrap();
        }
        let mut cursor = io::Cursor::new(wire);
        for m in &msgs {
            assert_eq!(&read_message(&mut cursor).unwrap(), m);
        }
    }

    #[test]
    fn rejects_oversized_frame() {
        let mut wire = Vec::new();
        wire.extend_from_slice(&(u32::MAX).to_le_bytes());
        assert!(read_message(&mut io::Cursor::new(wire)).is_err());
    }
}
