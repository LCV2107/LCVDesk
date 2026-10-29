//! Captura de pantalla en Windows, con selección de monitor.
//!
//! Igual que AnyDesk y RustDesk (`libs/scrap/src/dxgi`): DXGI Desktop Duplication como método principal
//! y GDI (`BitBlt`) como respaldo cuando DXGI no está disponible.

mod cursor;
mod dxgi;
mod gdi;
mod system_cursor;

use std::time::{Duration, Instant};
use tracing::{info, warn};

pub use dxgi::{Dxgi, enumerate};
pub use gdi::Gdi;
pub use system_cursor::{CursorImage, CursorState, cursor_image, cursor_state};

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

/// Miniatura RGBA de un fotograma BGRA que cabe en `max_w × max_h` sin deformarse. Cada píxel es la
/// media del bloque de origen que representa (se lee bien el texto grande y las ventanas).
pub fn thumbnail(frame: &Frame<'_>, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
    let (w, h) = (frame.width.max(1), frame.height.max(1));
    let scale = (max_w as f32 / w as f32).min(max_h as f32 / h as f32).min(1.0);
    let tw = ((w as f32 * scale).round() as u32).max(1);
    let th = ((h as f32 * scale).round() as u32).max(1);
    let stride = w as usize * 4;
    let mut out = Vec::with_capacity(tw as usize * th as usize * 4);
    for ty in 0..th {
        let y0 = (ty * h / th) as usize;
        let y1 = (((ty + 1) * h / th) as usize).max(y0 + 1);
        for tx in 0..tw {
            let x0 = (tx * w / tw) as usize;
            let x1 = (((tx + 1) * w / tw) as usize).max(x0 + 1);
            let (mut b, mut g, mut r, mut n) = (0u32, 0u32, 0u32, 0u32);
            for y in y0..y1.min(h as usize) {
                let row = &frame.data[y * stride..];
                for x in x0..x1.min(w as usize) {
                    let p = &row[x * 4..x * 4 + 4];
                    b += p[0] as u32;
                    g += p[1] as u32;
                    r += p[2] as u32;
                    n += 1;
                }
            }
            let n = n.max(1);
            out.extend_from_slice(&[(r / n) as u8, (g / n) as u8, (b / n) as u8, 255]);
        }
    }
    (tw, th, out)
}

/// Intervalo entre reintentos de DXGI mientras se usa GDI.
const DXGI_RETRY: Duration = Duration::from_secs(2);

pub struct Capturer {
    display: DisplayInfo,
    dxgi: Option<Dxgi>,
    gdi: Option<Gdi>,
    retry_dxgi_at: Instant,
    last: Backend,
    /// Pintar el puntero dentro de la imagen (visores antiguos). Si no, va aparte.
    with_cursor: bool,
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
            with_cursor: true,
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

    /// `false`: el puntero no se pinta en la imagen (el visor lo dibuja aparte) y moverlo no produce
    /// fotogramas nuevos.
    pub fn set_with_cursor(&mut self, on: bool) {
        self.with_cursor = on;
        if let Some(d) = self.dxgi.as_mut() {
            d.set_with_cursor(on);
        }
        if let Some(g) = self.gdi.as_mut() {
            g.with_cursor = on;
        }
    }

    pub fn backend(&self) -> Backend {
        if self.dxgi.is_some() { Backend::Dxgi } else { Backend::Gdi }
    }

    fn start_gdi(&mut self) -> Result<(), CaptureError> {
        let mut gdi = Gdi::new(self.display.x, self.display.y, self.display.width, self.display.height)?;
        gdi.with_cursor = self.with_cursor;
        self.gdi = Some(gdi);
        Ok(())
    }

    /// Espera hasta `timeout` un fotograma nuevo. `Ok(None)` = la pantalla no cambió.
    pub fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame<'_>>, CaptureError> {
        if self.dxgi.is_none() && Instant::now() >= self.retry_dxgi_at {
            match Dxgi::new(self.display.index) {
                Ok(mut d) => {
                    d.set_with_cursor(self.with_cursor);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnail_keeps_aspect_and_averages() {
        // 4x2 BGRA: mitad izquierda blanca, mitad derecha negra → 2x1 = blanco, negro.
        let mut data = Vec::new();
        for _ in 0..2 {
            for x in 0..4 {
                let v = if x < 2 { 255 } else { 0 };
                data.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let frame = Frame { width: 4, height: 2, data: &data };
        let (w, h, rgba) = thumbnail(&frame, 2, 2);
        assert_eq!((w, h), (2, 1));
        assert_eq!(rgba, [255, 255, 255, 255, 0, 0, 0, 255]);
        // Un 1920x1080 cabe en 240x180 como 240x135.
        let big = vec![0u8; 1920 * 1080 * 4];
        let (w, h, rgba) = thumbnail(&Frame { width: 1920, height: 1080, data: &big }, 240, 180);
        assert_eq!((w, h, rgba.len()), (240, 135, 240 * 135 * 4));
    }
}
