//! Mensajes del protocolo LCVDesk y su tramado sobre un flujo de bytes (TCP).
//!
//! Trama: `[u32 little-endian: longitud][mensaje serializado con postcard]`.

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

/// Versión que habla esta compilación. 3 añade el audio (`Audio`, `AudioEnable`) y las miniaturas
/// de monitores (`PreviewRequest`, `Preview`).
pub const PROTOCOL_VERSION: u32 = 3;
/// Versión más antigua con la que se sigue hablando (sin audio). Un host acepta cualquier visor
/// entre esta y `PROTOCOL_VERSION`; un visor nuevo que choca con un host antiguo reintenta con esta.
pub const MIN_PROTOCOL_VERSION: u32 = 2;
/// Primera versión con audio: solo se envían mensajes de audio si ambos lados la alcanzan.
pub const AUDIO_PROTOCOL_VERSION: u32 = 3;
/// Motivo exacto con el que un host rechaza una versión que no admite (los hosts antiguos ya lo
/// usaban): el visor lo reconoce para reintentar con `MIN_PROTOCOL_VERSION`.
pub const REJECT_VERSION: &str = "Versión de LCVDesk incompatible";
pub const DEFAULT_PORT: u16 = 47070;

/// Descubrimiento en LAN: grupo multicast y puerto reservados para LCVDesk.
pub const DISCOVERY_ADDR: [u8; 4] = [239, 255, 47, 71];
pub const DISCOVERY_PORT: u16 = 47071;
/// Marca al inicio del datagrama de descubrimiento ("LCVD") para ignorar tráfico ajeno.
const DISCOVERY_MAGIC: u32 = 0x4C_43_56_44;
/// Versión del formato de descubrimiento, independiente de `PROTOCOL_VERSION`.
const DISCOVERY_VERSION: u8 = 1;

/// Límite de seguridad para una trama (un fotograma 4K intra cabe holgadamente).
const MAX_MESSAGE_LEN: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Message {
    /// Visor → host: primer mensaje de la conexión.
    Hello { version: u32, name: String },
    /// Host → visor: el host tiene contraseña configurada. El visor responde `AuthResponse`
    /// con `HMAC-SHA256(contraseña, nonce)`. La contraseña nunca viaja por la red.
    AuthChallenge { nonce: Vec<u8> },
    /// Visor → host: respuesta al reto de contraseña.
    AuthResponse { mac: Vec<u8> },
    /// Host → visor: el usuario aceptó. Incluye la identidad del host (para guardarlo en "Recientes"),
    /// la lista de monitores y cuál se transmite al principio.
    Accepted { host_id: String, host_name: String, displays: Vec<DisplayInfo>, current: u32 },
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

    // --- Desde la versión 3 (solo si ambos lados la alcanzan; van al final para no cambiar los
    // índices de las variantes anteriores en postcard) ---
    /// Host → visor: un trozo del sonido que se reproduce en el equipo remoto.
    Audio(AudioChunk),
    /// Visor → host: empezar o dejar de enviar el sonido (botón de silencio del visor).
    AudioEnable { on: bool },
    /// Visor → host: pedir una miniatura de un monitor (al pasar el cursor por su botón).
    PreviewRequest { index: u32 },
    /// Host → visor: miniatura de un monitor.
    Preview(DisplayPreview),
}

/// Miniatura de un monitor del host (≤ 240×180), en RGBA de 8 bits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayPreview {
    pub index: u32,
    pub width: u32,
    pub height: u32,
    #[serde(with = "serde_bytes")]
    pub rgba: Vec<u8>,
}

/// Sonido del host: PCM de 16 bits con signo, little-endian, canales intercalados. Sin comprimir:
/// en red local son ~1,5 Mbps a 48 kHz estéreo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioChunk {
    pub sample_rate: u32,
    pub channels: u16,
    #[serde(with = "serde_bytes")]
    pub pcm: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    /// Botón lateral «atrás» (X1).
    Back,
    /// Botón lateral «adelante» (X2).
    Forward,
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

// --- Descubrimiento en LAN (UDP multicast) ---

/// Datos que un equipo anuncia en la red para que otros lo encuentren. No contiene nada sensible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Beacon {
    /// Identificador estable del equipo (no cambia aunque cambie la IP).
    pub id: String,
    pub name: String,
    /// Puerto TCP donde atiende conexiones.
    pub port: u16,
    pub version: String,
    /// `true` si ya tiene una sesión activa (ocupado).
    pub busy: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DiscoveryPacket {
    /// "Estoy aquí" (se emite periódicamente y como respuesta a `Query`).
    Announce(Beacon),
    /// "¿Quién hay?" (al arrancar, para no esperar al siguiente anuncio).
    Query,
}

/// Serializa un paquete de descubrimiento con su marca y versión al frente.
pub fn encode_discovery(packet: &DiscoveryPacket) -> Vec<u8> {
    let mut buf = Vec::with_capacity(96);
    buf.extend_from_slice(&DISCOVERY_MAGIC.to_le_bytes());
    buf.push(DISCOVERY_VERSION);
    postcard::to_extend(packet, buf).unwrap_or_default()
}

/// Decodifica un datagrama de descubrimiento; `None` si no es nuestro o está corrupto.
pub fn decode_discovery(data: &[u8]) -> Option<DiscoveryPacket> {
    if data.len() < 5 {
        return None;
    }
    if u32::from_le_bytes([data[0], data[1], data[2], data[3]]) != DISCOVERY_MAGIC || data[4] != DISCOVERY_VERSION {
        return None;
    }
    postcard::from_bytes(&data[5..]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Los visores y hosts de la versión 2 decodifican por índice de variante: los antiguos no pueden
    /// moverse. Si este test falla, se ha insertado una variante en medio del enum.
    #[test]
    fn v2_variant_indexes_unchanged() {
        assert_eq!(postcard::to_allocvec(&Message::RequestKeyframe).unwrap(), [8]);
        assert_eq!(postcard::to_allocvec(&Message::ReleaseAll).unwrap(), [16]);
        assert_eq!(postcard::to_allocvec(&Message::AudioEnable { on: true }).unwrap(), [18, 1]);
    }

    #[test]
    fn audio_roundtrip() {
        let msgs = [
            Message::AudioEnable { on: false },
            Message::Audio(AudioChunk { sample_rate: 48_000, channels: 2, pcm: vec![1, 0, 255, 127, 0, 128] }),
            Message::PreviewRequest { index: 1 },
            Message::Preview(DisplayPreview { index: 1, width: 1, height: 1, rgba: vec![10, 20, 30, 255] }),
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
    fn roundtrip() {
        let msgs = [
            Message::Hello { version: PROTOCOL_VERSION, name: "PC-1".into() },
            Message::Ping { t_us: 123_456 },
            Message::MouseMove { x: 32768, y: 100 },
            Message::Key { scancode: 0x1c, extended: false, down: true },
            Message::Accepted {
                host_id: "ab12cd34".into(),
                host_name: "PC-TALLER".into(),
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
    fn discovery_roundtrip() {
        let beacon = Beacon { id: "ab12".into(), name: "PC-TALLER".into(), port: 47070, version: "0.2.0".into(), busy: false };
        let pkt = DiscoveryPacket::Announce(beacon);
        assert_eq!(decode_discovery(&encode_discovery(&pkt)), Some(pkt));
        assert_eq!(decode_discovery(&encode_discovery(&DiscoveryPacket::Query)), Some(DiscoveryPacket::Query));
        // Un datagrama ajeno (sin nuestra marca) se ignora.
        assert_eq!(decode_discovery(b"basura cualquiera"), None);
    }

    #[test]
    fn rejects_oversized_frame() {
        let mut wire = Vec::new();
        wire.extend_from_slice(&(u32::MAX).to_le_bytes());
        assert!(read_message(&mut io::Cursor::new(wire)).is_err());
    }
}
