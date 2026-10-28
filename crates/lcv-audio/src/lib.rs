//! Sonido del equipo remoto con WASAPI.
//!
//! - [`Loopback`] (host): captura lo que suena por los altavoces del equipo (modo *loopback* del
//!   dispositivo de salida por defecto) y lo entrega en trozos de 20 ms, en PCM de 16 bits estéreo.
//! - [`Player`] (visor): reproduce esos trozos por la salida por defecto, con un colchón pequeño
//!   para que no se corte ni acumule retraso.
//!
//! Sin compresión: en red local ~1,5 Mbps a 48 kHz no compensan un códec en C.

mod capture;
mod playback;

pub use capture::Loopback;
pub use playback::Player;

/// Un trozo de sonido: PCM de 16 bits con signo, canales intercalados.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<i16>,
}

impl Chunk {
    /// Muestras como bytes little-endian (lo que viaja por la red).
    pub fn to_le_bytes(&self) -> Vec<u8> {
        self.samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    /// Reconstruye un trozo a partir de bytes little-endian (se ignora un byte suelto final).
    pub fn from_le_bytes(sample_rate: u32, channels: u16, pcm: &[u8]) -> Self {
        let samples = pcm.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect();
        Self { sample_rate, channels, samples }
    }

    /// Número de fotogramas de audio (muestras por canal).
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1) as usize
    }
}

/// Identificador del dispositivo de salida por defecto (para detectar que el usuario cambió de
/// altavoces a auriculares y reabrir).
fn default_render_id(enumerator: &windows::Win32::Media::Audio::IMMDeviceEnumerator) -> Option<String> {
    use windows::Win32::Media::Audio::{eConsole, eRender};
    use windows::Win32::System::Com::CoTaskMemFree;
    unsafe {
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
        let id = device.GetId().ok()?;
        let text = id.to_string().ok();
        CoTaskMemFree(Some(id.0 as *const _));
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_roundtrip() {
        let c = Chunk { sample_rate: 48_000, channels: 2, samples: vec![0, -1, i16::MAX, i16::MIN] };
        assert_eq!(Chunk::from_le_bytes(48_000, 2, &c.to_le_bytes()), c);
        assert_eq!(c.frames(), 2);
    }
}
