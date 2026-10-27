//! Traducción de las teclas lógicas de egui a scancodes de hardware de Windows.
//!
//! El visor también corre en Windows, así que convierte la tecla → código virtual (VK) → scancode con
//! `MapVirtualKeyW`. Así el host solo inyecta scancodes, independientes de la distribución del teclado.

use eframe::egui::Key;
use windows::Win32::UI::Input::KeyboardAndMouse::{MAPVK_VK_TO_VSC_EX, MapVirtualKeyW};

/// Códigos virtuales de los modificadores (para enviarlos al cambiar su estado).
pub const VK_SHIFT: u16 = 0x10;
pub const VK_CONTROL: u16 = 0x11;
pub const VK_MENU: u16 = 0x12; // Alt

/// Convierte un VK en (scancode, ¿tecla extendida?). Las teclas extendidas (flechas, Inicio, etc.)
/// vienen marcadas en el byte alto que devuelve `MapVirtualKeyW` con `MAPVK_VK_TO_VSC_EX`.
pub fn vk_to_scancode(vk: u16) -> Option<(u16, bool)> {
    let sc = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC_EX) };
    if sc == 0 {
        return None;
    }
    let extended = (sc & 0xE000) == 0xE000;
    Some(((sc & 0xFF) as u16, extended))
}

/// VK correspondiente a una tecla de egui, o `None` si no se controla.
pub fn egui_key_to_vk(key: Key) -> Option<u16> {
    let vk = match key {
        Key::ArrowDown => 0x28,
        Key::ArrowLeft => 0x25,
        Key::ArrowRight => 0x27,
        Key::ArrowUp => 0x26,
        Key::Escape => 0x1B,
        Key::Tab => 0x09,
        Key::Backspace => 0x08,
        Key::Enter => 0x0D,
        Key::Space => 0x20,
        Key::Insert => 0x2D,
        Key::Delete => 0x2E,
        Key::Home => 0x24,
        Key::End => 0x23,
        Key::PageUp => 0x21,
        Key::PageDown => 0x22,
        Key::Copy => 0x2E,
        Key::Cut => 0x2E,
        Key::Paste => 0x2D,
        Key::Colon | Key::Semicolon => 0xBA,
        Key::Comma => 0xBC,
        Key::Backslash | Key::Pipe => 0xDC,
        Key::Slash | Key::Questionmark => 0xBF,
        Key::OpenBracket => 0xDB,
        Key::CloseBracket => 0xDD,
        Key::Backtick => 0xC0,
        Key::Minus => 0xBD,
        Key::Period => 0xBE,
        Key::Plus | Key::Equals => 0xBB,
        Key::Quote => 0xDE,
        Key::Num0 => 0x30,
        Key::Num1 => 0x31,
        Key::Num2 => 0x32,
        Key::Num3 => 0x33,
        Key::Num4 => 0x34,
        Key::Num5 => 0x35,
        Key::Num6 => 0x36,
        Key::Num7 => 0x37,
        Key::Num8 => 0x38,
        Key::Num9 => 0x39,
        Key::A => 0x41,
        Key::B => 0x42,
        Key::C => 0x43,
        Key::D => 0x44,
        Key::E => 0x45,
        Key::F => 0x46,
        Key::G => 0x47,
        Key::H => 0x48,
        Key::I => 0x49,
        Key::J => 0x4A,
        Key::K => 0x4B,
        Key::L => 0x4C,
        Key::M => 0x4D,
        Key::N => 0x4E,
        Key::O => 0x4F,
        Key::P => 0x50,
        Key::Q => 0x51,
        Key::R => 0x52,
        Key::S => 0x53,
        Key::T => 0x54,
        Key::U => 0x55,
        Key::V => 0x56,
        Key::W => 0x57,
        Key::X => 0x58,
        Key::Y => 0x59,
        Key::Z => 0x5A,
        Key::F1 => 0x70,
        Key::F2 => 0x71,
        Key::F3 => 0x72,
        Key::F4 => 0x73,
        Key::F5 => 0x74,
        Key::F6 => 0x75,
        Key::F7 => 0x76,
        Key::F8 => 0x77,
        Key::F9 => 0x78,
        Key::F10 => 0x79,
        Key::F11 => 0x7A,
        Key::F12 => 0x7B,
        _ => return None,
    };
    Some(vk)
}

/// ¿La tecla produce texto imprimible? En ese caso se envía como Unicode y no como scancode
/// (salvo que haya Ctrl/Alt/Win, que convierten la pulsación en un atajo).
pub fn is_text_key(key: Key) -> bool {
    matches!(key,
        Key::A | Key::B | Key::C | Key::D | Key::E | Key::F | Key::G | Key::H | Key::I | Key::J
        | Key::K | Key::L | Key::M | Key::N | Key::O | Key::P | Key::Q | Key::R | Key::S | Key::T
        | Key::U | Key::V | Key::W | Key::X | Key::Y | Key::Z
        | Key::Num0 | Key::Num1 | Key::Num2 | Key::Num3 | Key::Num4 | Key::Num5 | Key::Num6
        | Key::Num7 | Key::Num8 | Key::Num9 | Key::Space
        | Key::Colon | Key::Semicolon | Key::Comma | Key::Backslash | Key::Pipe | Key::Slash
        | Key::Questionmark | Key::OpenBracket | Key::CloseBracket | Key::Backtick | Key::Minus
        | Key::Period | Key::Plus | Key::Equals | Key::Quote
    )
}
