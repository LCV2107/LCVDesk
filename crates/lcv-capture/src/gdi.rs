//! Captura GDI (`BitBlt`) de un monitor concreto. Más lenta que DXGI y sin detección de cambios
//! por hardware: se compara con el fotograma anterior para no reenviar imágenes idénticas.

use crate::{CaptureError, Frame};
use std::ffi::c_void;
use windows::Win32::Graphics::Gdi::*;

pub struct Gdi {
    /// Esquina del monitor dentro del escritorio virtual.
    origin_x: i32,
    origin_y: i32,
    width: u32,
    height: u32,
    screen_dc: HDC,
    mem_dc: HDC,
    bitmap: HBITMAP,
    old_bitmap: HGDIOBJ,
    bits: *const u8,
    buf: Vec<u8>,
    has_frame: bool,
    /// (handle del cursor, punto activo x, y) del último cursor dibujado.
    cursor_hotspot: (isize, i32, i32),
    /// Pintar el puntero en la imagen (si no, el visor lo dibuja aparte).
    pub(crate) with_cursor: bool,
}

// Los handles GDI pertenecen a este objeto y solo se usan desde el hilo que lo posee.
unsafe impl Send for Gdi {}

impl Gdi {
    pub fn new(origin_x: i32, origin_y: i32, width: u32, height: u32) -> Result<Self, CaptureError> {
        if width == 0 || height == 0 {
            return Err(CaptureError::Gdi("tamaño de monitor inválido".into()));
        }
        unsafe {
            // DC del escritorio completo: permite copiar cualquier monitor por sus coordenadas virtuales.
            let screen_dc = GetDC(None);
            let mem_dc = CreateCompatibleDC(Some(screen_dc));
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width as i32,
                    biHeight: -(height as i32), // negativo = filas de arriba abajo
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bitmap = match CreateDIBSection(Some(mem_dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(b) => b,
                Err(e) => {
                    let _ = DeleteDC(mem_dc);
                    ReleaseDC(None, screen_dc);
                    return Err(CaptureError::Gdi(e.to_string()));
                }
            };
            let old_bitmap = SelectObject(mem_dc, bitmap.into());
            Ok(Self {
                origin_x,
                origin_y,
                width,
                height,
                screen_dc,
                mem_dc,
                bitmap,
                old_bitmap,
                bits: bits as *const u8,
                buf: vec![0; width as usize * height as usize * 4],
                has_frame: false,
                cursor_hotspot: (0, 0, 0),
                with_cursor: true,
            })
        }
    }

    /// `Ok(true)` si la imagen cambió respecto a la anterior.
    pub fn capture(&mut self) -> Result<bool, CaptureError> {
        let (w, h) = (self.width as i32, self.height as i32);
        unsafe {
            BitBlt(
                self.mem_dc, 0, 0, w, h, Some(self.screen_dc), self.origin_x, self.origin_y,
                SRCCOPY | CAPTUREBLT,
            )
            .map_err(|e| CaptureError::Gdi(e.to_string()))?;
            // BitBlt no copia el puntero: se dibuja encima (y así moverlo cuenta como cambio).
            if self.with_cursor {
                self.draw_cursor();
            }
            let _ = GdiFlush();
            // SAFETY: la sección DIB tiene exactamente ancho × alto × 4 bytes y vive mientras `self`.
            let src = std::slice::from_raw_parts(self.bits, self.buf.len());
            if self.has_frame && src == self.buf.as_slice() {
                return Ok(false);
            }
            self.buf.copy_from_slice(src);
        }
        self.has_frame = true;
        Ok(true)
    }

    /// Dibuja el puntero del sistema en el bitmap de memoria, en su posición dentro de este monitor.
    unsafe fn draw_cursor(&mut self) {
        use windows::Win32::UI::WindowsAndMessaging::{
            CURSOR_SHOWING, CURSORINFO, DI_NORMAL, DrawIconEx, GetCursorInfo, GetIconInfo, HICON, ICONINFO,
        };
        let mut ci = CURSORINFO { cbSize: size_of::<CURSORINFO>() as u32, ..Default::default() };
        if unsafe { GetCursorInfo(&mut ci) }.is_err() || ci.flags.0 & CURSOR_SHOWING.0 == 0 || ci.hCursor.is_invalid() {
            return;
        }
        let icon = HICON(ci.hCursor.0);
        // Punto activo del cursor (la punta de la flecha), en caché por cursor: GetIconInfo crea
        // bitmaps que hay que liberar y no compensa hacerlo en cada fotograma.
        if self.cursor_hotspot.0 != ci.hCursor.0 as isize {
            let mut ii = ICONINFO::default();
            if unsafe { GetIconInfo(icon, &mut ii) }.is_err() {
                return;
            }
            unsafe {
                if !ii.hbmMask.is_invalid() {
                    let _ = DeleteObject(ii.hbmMask.into());
                }
                if !ii.hbmColor.is_invalid() {
                    let _ = DeleteObject(ii.hbmColor.into());
                }
            }
            self.cursor_hotspot = (ci.hCursor.0 as isize, ii.xHotspot as i32, ii.yHotspot as i32);
        }
        let x = ci.ptScreenPos.x - self.origin_x - self.cursor_hotspot.1;
        let y = ci.ptScreenPos.y - self.origin_y - self.cursor_hotspot.2;
        let _ = unsafe { DrawIconEx(self.mem_dc, x, y, icon, 0, 0, 0, None, DI_NORMAL) };
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn frame(&self) -> Frame<'_> {
        Frame { width: self.width, height: self.height, data: &self.buf }
    }
}

impl Drop for Gdi {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.mem_dc, self.old_bitmap);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.mem_dc);
            ReleaseDC(None, self.screen_dc);
        }
    }
}
