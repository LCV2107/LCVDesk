//! Puntero del ratón sobre la imagen capturada.
//!
//! DXGI Desktop Duplication entrega el escritorio **sin** el puntero (va en una capa aparte del
//! hardware); sin dibujarlo, el que controla no ve dónde está el ratón remoto ni su forma (flecha,
//! cursor de texto, mano...). Aquí se dibuja sobre el buffer BGRA antes de codificar, siguiendo las
//! reglas de la muestra oficial de Desktop Duplication para los tres tipos de forma.
//!
//! Se guarda lo que había debajo del puntero para restaurarlo cuando solo se mueve el ratón: así no
//! hace falta volver a copiar el escritorio entero desde la GPU.

/// Tipo de forma, como lo da `DXGI_OUTDUPL_POINTER_SHAPE_TYPE`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShapeKind {
    /// 1 bpp: máscara AND (mitad superior) y XOR (mitad inferior). Permite invertir el fondo.
    Monochrome,
    /// 32 bpp BGRA con alfa.
    Color,
    /// 32 bpp BGR; el byte alto es máscara: 0 = sustituir, 0xFF = XOR con el fondo.
    MaskedColor,
}

#[derive(Clone, Debug)]
pub struct Shape {
    pub kind: ShapeKind,
    pub width: u32,
    /// Alto real del puntero (en monocromo, la mitad del alto de los datos).
    pub height: u32,
    pub pitch: u32,
    pub data: Vec<u8>,
}

/// Estado del puntero: esquina superior izquierda de la forma, en píxeles del monitor capturado.
#[derive(Clone, Debug, Default)]
pub struct Pointer {
    pub visible: bool,
    pub x: i32,
    pub y: i32,
    pub shape: Option<Shape>,
}

/// Puntero dibujado sobre un buffer, con lo que había debajo para poder quitarlo.
#[derive(Default)]
pub struct Overlay {
    /// (x, y, ancho, alto, píxeles originales) del rectángulo tapado.
    saved: Option<(usize, usize, usize, usize, Vec<u8>)>,
}

impl Overlay {
    /// El buffer se acaba de rellenar con una imagen limpia: no hay nada que restaurar.
    pub fn invalidate(&mut self) {
        self.saved = None;
    }

    /// Devuelve al buffer los píxeles que tapaba el puntero.
    pub fn restore(&mut self, buf: &mut [u8], width: u32) {
        let Some((x, y, w, h, pixels)) = self.saved.take() else { return };
        let stride = width as usize * 4;
        for row in 0..h {
            let dst = (y + row) * stride + x * 4;
            if let Some(line) = buf.get_mut(dst..dst + w * 4) {
                line.copy_from_slice(&pixels[row * w * 4..(row + 1) * w * 4]);
            }
        }
    }

    /// Dibuja el puntero (si es visible) y guarda lo que queda debajo. Llamar con el buffer limpio
    /// (tras `invalidate` o `restore`).
    pub fn draw(&mut self, buf: &mut [u8], width: u32, height: u32, pointer: &Pointer) {
        let Some(shape) = pointer.shape.as_ref().filter(|_| pointer.visible) else { return };
        // Recorte contra los bordes del monitor (el puntero puede salirse a medias).
        let x0 = pointer.x.max(0);
        let y0 = pointer.y.max(0);
        let x1 = (pointer.x + shape.width as i32).min(width as i32);
        let y1 = (pointer.y + shape.height as i32).min(height as i32);
        if x1 <= x0 || y1 <= y0 || buf.len() < width as usize * height as usize * 4 {
            return;
        }
        let (x0, y0, x1, y1) = (x0 as usize, y0 as usize, x1 as usize, y1 as usize);
        let (w, h) = (x1 - x0, y1 - y0);
        let stride = width as usize * 4;

        let mut saved = Vec::with_capacity(w * h * 4);
        for row in y0..y1 {
            saved.extend_from_slice(&buf[row * stride + x0 * 4..row * stride + x1 * 4]);
        }
        self.saved = Some((x0, y0, w, h, saved));

        // Coordenadas dentro de la forma del primer píxel visible.
        let sx0 = (x0 as i32 - pointer.x) as usize;
        let sy0 = (y0 as i32 - pointer.y) as usize;
        let pitch = shape.pitch as usize;
        for row in 0..h {
            let sy = sy0 + row;
            for col in 0..w {
                let sx = sx0 + col;
                let at = (y0 + row) * stride + (x0 + col) * 4;
                let dst = u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]);
                let out = match shape.kind {
                    ShapeKind::Monochrome => {
                        let bit = 0x80u8 >> (sx % 8);
                        let and = shape.data.get(sy * pitch + sx / 8).is_some_and(|b| b & bit != 0);
                        let xor = shape
                            .data
                            .get((sy + shape.height as usize) * pitch + sx / 8)
                            .is_some_and(|b| b & bit != 0);
                        let and_mask = if and { 0xFFFF_FFFF } else { 0xFF00_0000 };
                        let xor_mask = if xor { 0x00FF_FFFF } else { 0 };
                        ((dst & and_mask) ^ xor_mask) | 0xFF00_0000
                    }
                    ShapeKind::Color => {
                        let i = sy * pitch + sx * 4;
                        let Some(p) = shape.data.get(i..i + 4) else { continue };
                        let a = p[3] as u32;
                        let blend = |s: u8, d: u32| (s as u32 * a + d * (255 - a)) / 255;
                        let b = blend(p[0], dst & 0xFF);
                        let g = blend(p[1], (dst >> 8) & 0xFF);
                        let r = blend(p[2], (dst >> 16) & 0xFF);
                        0xFF00_0000 | (r << 16) | (g << 8) | b
                    }
                    ShapeKind::MaskedColor => {
                        let i = sy * pitch + sx * 4;
                        let Some(p) = shape.data.get(i..i + 4) else { continue };
                        let src = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
                        if p[3] == 0 { src | 0xFF00_0000 } else { (dst ^ src) | 0xFF00_0000 }
                    }
                };
                buf[at..at + 4].copy_from_slice(&out.to_le_bytes());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas(w: u32, h: u32, px: u32) -> Vec<u8> {
        (0..w * h).flat_map(|_| px.to_le_bytes()).collect()
    }

    fn pixel(buf: &[u8], w: u32, x: u32, y: u32) -> u32 {
        let i = ((y * w + x) * 4) as usize;
        u32::from_le_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]])
    }

    #[test]
    fn monochrome_rules_and_restore() {
        // 8x1: AND = 1010_0000, XOR = 1100_0000 → px0 invierte, px1 blanco, px2 transparente, px3 negro.
        let shape = Shape { kind: ShapeKind::Monochrome, width: 4, height: 1, pitch: 1, data: vec![0b1010_0000, 0b1100_0000] };
        let pointer = Pointer { visible: true, x: 1, y: 0, shape: Some(shape) };
        let mut buf = canvas(6, 1, 0xFF20_4060);
        let mut ov = Overlay::default();
        ov.draw(&mut buf, 6, 1, &pointer);
        assert_eq!(pixel(&buf, 6, 0, 0), 0xFF20_4060); // fuera del puntero
        assert_eq!(pixel(&buf, 6, 1, 0), 0xFFDF_BF9F); // invertido
        assert_eq!(pixel(&buf, 6, 2, 0), 0xFFFF_FFFF); // blanco
        assert_eq!(pixel(&buf, 6, 3, 0), 0xFF20_4060); // transparente
        assert_eq!(pixel(&buf, 6, 4, 0), 0xFF00_0000); // negro
        ov.restore(&mut buf, 6);
        assert_eq!(buf, canvas(6, 1, 0xFF20_4060));
    }

    #[test]
    fn color_blend_and_clipping() {
        // Puntero 2x2 de color medio transparente, medio salido por la esquina superior izquierda.
        let px = [0u8, 0, 255, 255]; // rojo opaco
        let shape = Shape { kind: ShapeKind::Color, width: 2, height: 2, pitch: 8, data: px.repeat(4) };
        let pointer = Pointer { visible: true, x: -1, y: -1, shape: Some(shape) };
        let mut buf = canvas(3, 3, 0xFF00_0000);
        let mut ov = Overlay::default();
        ov.draw(&mut buf, 3, 3, &pointer);
        assert_eq!(pixel(&buf, 3, 0, 0), 0xFFFF_0000);
        assert_eq!(pixel(&buf, 3, 1, 0), 0xFF00_0000);
        assert_eq!(pixel(&buf, 3, 0, 1), 0xFF00_0000);
    }

    #[test]
    fn hidden_pointer_draws_nothing() {
        let shape = Shape { kind: ShapeKind::Color, width: 1, height: 1, pitch: 4, data: vec![255; 4] };
        let pointer = Pointer { visible: false, x: 0, y: 0, shape: Some(shape) };
        let mut buf = canvas(2, 2, 0xFF11_2233);
        Overlay::default().draw(&mut buf, 2, 2, &pointer);
        assert_eq!(buf, canvas(2, 2, 0xFF11_2233));
    }
}
