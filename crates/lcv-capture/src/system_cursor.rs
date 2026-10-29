//! Puntero del sistema leído aparte de la imagen: dónde está y qué forma tiene, en RGBA.
//!
//! Con el puntero aparte, el visor lo dibuja encima a su tamaño real aunque la pantalla remota se
//! vea reducida (un 4K en una ventana pequeña), y moverlo no obliga a codificar otro fotograma.

use std::ffi::c_void;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::{CURSOR_SHOWING, CURSORINFO, GetCursorInfo, GetIconInfo, HICON, ICONINFO};

/// Estado del puntero: `id` identifica su forma (el handle del cursor, estable mientras exista).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorState {
    pub id: u64,
    /// Punto activo en coordenadas del escritorio virtual (píxeles físicos).
    pub x: i32,
    pub y: i32,
    pub visible: bool,
}

/// Forma de un puntero en RGBA sin premultiplicar.
#[derive(Clone, Debug)]
pub struct CursorImage {
    pub width: u32,
    pub height: u32,
    pub hot_x: u32,
    pub hot_y: u32,
    pub rgba: Vec<u8>,
}

/// Lado máximo que se acepta (los punteros grandes de accesibilidad llegan a 256 px).
const MAX_SIDE: u32 = 256;

pub fn cursor_state() -> Option<CursorState> {
    let mut ci = CURSORINFO { cbSize: size_of::<CURSORINFO>() as u32, ..Default::default() };
    unsafe { GetCursorInfo(&mut ci) }.ok()?;
    Some(CursorState {
        id: ci.hCursor.0 as usize as u64,
        x: ci.ptScreenPos.x,
        y: ci.ptScreenPos.y,
        visible: ci.flags.0 & CURSOR_SHOWING.0 != 0 && !ci.hCursor.is_invalid(),
    })
}

/// Convierte el cursor `id` (de [`cursor_state`]) a RGBA. Los píxeles que en Windows invierten el
/// fondo (el cursor de texto) no se pueden representar en RGBA: se pintan negros con un borde
/// blanco, que se ve sobre fondos claros y oscuros.
pub fn cursor_image(id: u64) -> Option<CursorImage> {
    let icon = HICON(id as usize as *mut c_void);
    let mut ii = ICONINFO::default();
    unsafe { GetIconInfo(icon, &mut ii) }.ok()?;
    let mask = read_bitmap(ii.hbmMask);
    let color = (!ii.hbmColor.is_invalid()).then(|| read_bitmap(ii.hbmColor)).flatten();
    unsafe {
        if !ii.hbmMask.is_invalid() {
            let _ = DeleteObject(ii.hbmMask.into());
        }
        if !ii.hbmColor.is_invalid() {
            let _ = DeleteObject(ii.hbmColor.into());
        }
    }
    let (mw, mh, mask) = mask?;
    let (hot_x, hot_y) = (ii.xHotspot, ii.yHotspot);
    let (w, h, px): (u32, u32, Vec<Px>) = match color {
        Some((cw, ch, color)) => {
            let has_alpha = color.iter().any(|p| p >> 24 != 0);
            let px = (0..(cw * ch) as usize)
                .map(|i| {
                    let c = color[i];
                    if has_alpha {
                        return Px::Rgba(c);
                    }
                    let and = mask.get(i).is_some_and(|m| m & 0xFF_FFFF != 0);
                    match (and, c & 0xFF_FFFF != 0) {
                        (false, _) => Px::Rgba(c | 0xFF00_0000),
                        (true, false) => Px::Clear,
                        (true, true) => Px::Invert,
                    }
                })
                .collect();
            (cw, ch, px)
        }
        // Monocromo: máscara AND arriba y XOR abajo, cada una de la mitad del alto.
        None => {
            let h = mh / 2;
            let px = (0..(mw * h) as usize)
                .map(|i| {
                    let and = mask[i] & 0xFF_FFFF != 0;
                    let xor = mask[i + (mw * h) as usize] & 0xFF_FFFF != 0;
                    match (and, xor) {
                        (false, false) => Px::Rgba(0xFF00_0000),
                        (false, true) => Px::Rgba(0xFFFF_FFFF),
                        (true, false) => Px::Clear,
                        (true, true) => Px::Invert,
                    }
                })
                .collect();
            (mw, h, px)
        }
    };
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
        return None;
    }
    Some(CursorImage { width: w, height: h, hot_x, hot_y, rgba: to_rgba(w, h, &px) })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Px {
    /// BGRA tal como lo da GDI (alfa en el byte alto).
    Rgba(u32),
    Clear,
    Invert,
}

fn to_rgba(w: u32, h: u32, px: &[Px]) -> Vec<u8> {
    let (w, h) = (w as i32, h as i32);
    let at = |x: i32, y: i32| (x >= 0 && y >= 0 && x < w && y < h).then(|| px[(y * w + x) as usize]);
    let mut out = Vec::with_capacity(px.len() * 4);
    for y in 0..h {
        for x in 0..w {
            let bgra = match px[(y * w + x) as usize] {
                Px::Rgba(c) => c,
                Px::Invert => 0xFF00_0000,
                // Borde blanco alrededor de lo que invertía el fondo.
                Px::Clear if [(-1, 0), (1, 0), (0, -1), (0, 1)].iter().any(|(dx, dy)| at(x + dx, y + dy) == Some(Px::Invert)) => {
                    0xFFFF_FFFF
                }
                Px::Clear => 0,
            };
            let [b, g, r, a] = bgra.to_le_bytes();
            out.extend_from_slice(&[r, g, b, a]);
        }
    }
    out
}

/// Píxeles de un bitmap GDI como BGRA de 32 bits, filas de arriba abajo.
fn read_bitmap(bitmap: HBITMAP) -> Option<(u32, u32, Vec<u32>)> {
    if bitmap.is_invalid() {
        return None;
    }
    let mut bm = BITMAP::default();
    if unsafe { GetObjectW(bitmap.into(), size_of::<BITMAP>() as i32, Some((&raw mut bm).cast())) } == 0 {
        return None;
    }
    let (w, h) = (bm.bmWidth.max(0) as u32, bm.bmHeight.unsigned_abs());
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE * 2 {
        return None;
    }
    let mut bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut buf = vec![0u32; (w * h) as usize];
    let dc = unsafe { GetDC(None) };
    let lines = unsafe { GetDIBits(dc, bitmap, 0, h, Some(buf.as_mut_ptr().cast()), &mut bmi, DIB_RGB_COLORS) };
    unsafe { ReleaseDC(None, dc) };
    (lines == h as i32).then_some((w, h, buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invert_pixels_get_white_outline() {
        // 3x1: transparente, invertir, transparente → blanco, negro, blanco.
        let rgba = to_rgba(3, 1, &[Px::Clear, Px::Invert, Px::Clear]);
        assert_eq!(rgba, [255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255]);
        // Lo transparente sin vecinos que inviertan sigue transparente; BGRA → RGBA.
        let rgba = to_rgba(2, 1, &[Px::Clear, Px::Rgba(0x8011_2233)]);
        assert_eq!(rgba, [0, 0, 0, 0, 0x11, 0x22, 0x33, 0x80]);
    }

    #[test]
    fn reads_the_current_cursor() {
        // En una sesión con escritorio siempre hay un puntero (aunque esté oculto).
        let Some(state) = cursor_state() else { return };
        if state.id == 0 {
            return;
        }
        let img = cursor_image(state.id).expect("forma del puntero");
        assert_eq!(img.rgba.len(), (img.width * img.height * 4) as usize);
        assert!(img.hot_x < img.width && img.hot_y < img.height);
        assert!(img.rgba.chunks(4).any(|p| p[3] != 0), "el puntero no puede ser todo transparente");
    }
}
