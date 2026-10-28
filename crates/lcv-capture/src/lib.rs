//! Captura de pantalla en Windows, con selección de monitor.
//!
//! Igual que AnyDesk y RustDesk (`libs/scrap/src/dxgi`): DXGI Desktop Duplication como método principal
//! y GDI (`BitBlt`) como respaldo cuando DXGI no está disponible.

mod cursor;
mod dxgi;
mod gdi;

use std::time::{Duration, Instant};
use tracing::{info, warn};

pub use dxgi::{Dxgi, enumerate};
pub use gdi::Gdi;

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("DXGI: {0}")]
    Dxgi(#[from] windows::core::Error),
    #[error("GDI: {0}")]
    Gdi(String),
    #[error("no se encontró ningún monitor")]
    NoPrimaryOutput,
}

/// Descripción de un monitor del equipo.
#[derive(Debug, Clone)]
pub struct DisplayInfo {
    pub index: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
    pub primary: bool,
}

/// Fotograma BGRA compacto (stride = ancho × 4).
pub struct Frame<'a> {
    pub width: u32,
    pub height: u32,
    pub data: &'a [u8],
}

/// Intervalo entre reintentos de DXGI mientras se usa GDI.
const DXGI_RETRY: Duration = Duration::from_secs(2);

pub struct Capturer {
    display: DisplayInfo,
    dxgi: Option<Dxgi>,
    gdi: Option<Gdi>,
    retry_dxgi_at: Instant,
    last: Backend,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Backend {
    None,
    Dxgi,
    Gdi,
}

impl Capturer {
    /// Captura el monitor indicado. `enumerate()` da la lista; índice fuera de rango → monitor principal.
    pub fn new(display_index: u32) -> Result<Self, CaptureError> {
        let displays = enumerate()?;
        let chosen = displays
            .iter()
            .find(|d| d.index == display_index)
            .or_else(|| displays.iter().find(|d| d.primary))
            .unwrap_or(&displays[0])
            .clone();
        let idx = chosen.index;
        let mut c = Self {
            display: chosen,
            dxgi: None,
            gdi: None,
            retry_dxgi_at: Instant::now(),
            last: Backend::None,
        };
        match Dxgi::new(idx) {
            Ok(d) => {
                let (w, h) = (d.width(), d.height());
                info!("captura DXGI monitor {idx} {w}x{h}");
                c.dxgi = Some(d);
            }
            Err(e) => {
                warn!("DXGI no disponible ({e}); usando GDI");
                c.start_gdi()?;
                c.retry_dxgi_at = Instant::now() + DXGI_RETRY;
            }
        }
        Ok(c)
    }

    pub fn display(&self) -> &DisplayInfo {
        &self.display
    }

    /// Tamaño actual de la pantalla capturada.
    pub fn size(&self) -> (u32, u32) {
        match (&self.dxgi, &self.gdi) {
            (Some(d), _) => (d.width(), d.height()),
            (None, Some(g)) => (g.width(), g.height()),
            (None, None) => (self.display.width, self.display.height),
        }
    }

    pub fn backend(&self) -> Backend {
        if self.dxgi.is_some() { Backend::Dxgi } else { Backend::Gdi }
    }

    fn start_gdi(&mut self) -> Result<(), CaptureError> {
        self.gdi = Some(Gdi::new(self.display.x, self.display.y, self.display.width, self.display.height)?);
        Ok(())
    }

    /// Espera hasta `timeout` un fotograma nuevo. `Ok(None)` = la pantalla no cambió.
    pub fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame<'_>>, CaptureError> {
        if self.dxgi.is_none() && Instant::now() >= self.retry_dxgi_at {
            match Dxgi::new(self.display.index) {
                Ok(d) => {
                    info!("DXGI recuperado monitor {} {}x{}", self.display.index, d.width(), d.height());
                    self.dxgi = Some(d);
                    self.gdi = None;
                }
                Err(_) => self.retry_dxgi_at = Instant::now() + DXGI_RETRY,
            }
        }

        let got = if let Some(d) = self.dxgi.as_mut() {
            match d.acquire(timeout) {
                Ok(changed) => changed.then_some(Backend::Dxgi),
                Err(e) => {
                    // ACCESS_LOST (cambio de modo, escritorio seguro, UAC...): se recrea en el siguiente
                    // intento; mientras tanto GDI.
                    warn!("DXGI interrumpido ({e}); cambiando a GDI");
                    self.dxgi = None;
                    self.retry_dxgi_at = Instant::now() + Duration::from_millis(500);
                    if self.gdi.is_none() {
                        self.start_gdi()?;
                    }
                    None
                }
            }
        } else if let Some(g) = self.gdi.as_mut() {
            std::thread::sleep(timeout);
            g.capture()?.then_some(Backend::Gdi)
        } else {
            None
        };

        if let Some(b) = got {
            self.last = b;
        }
        Ok(got.and_then(|_| self.last_frame()))
    }

    /// Último fotograma capturado (para reenviarlo como IDR si la pantalla está quieta).
    pub fn last_frame(&self) -> Option<Frame<'_>> {
        match self.last {
            Backend::Dxgi => self.dxgi.as_ref().and_then(|d| d.frame()),
            Backend::Gdi => self.gdi.as_ref().map(|g| g.frame()),
            Backend::None => None,
        }
    }
}
