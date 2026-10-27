//! Inyección de ratón y teclado en el host con `SendInput`.
//!
//! Adaptado del enfoque de RustDesk `libs/enigo/src/win` (AGPL-3.0): coordenadas absolutas de ratón
//! sobre el escritorio virtual y teclado por scancode. Todos los eventos se marcan con `EXTRA_INFO`
//! para poder distinguirlos de la entrada física si hiciera falta.

use lcv_proto::{Message, MouseButton};
use std::collections::HashSet;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

/// Marca de origen de los eventos sintéticos de LCVDesk.
pub const EXTRA_INFO: usize = 0x1C_7D_E5_00;

/// Valores de `mouseData` para los botones laterales del ratón (ver WinUser.h).
const XBUTTON1: i32 = 0x0001;
const XBUTTON2: i32 = 0x0002;

/// Rectángulo del monitor que se está controlando, en píxeles del escritorio virtual.
#[derive(Clone, Copy)]
pub struct TargetRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Inyector de entrada. Recuerda las teclas y botones pulsados para poder soltarlos todos al terminar.
pub struct Injector {
    target: TargetRect,
    keys_down: HashSet<u16>,
    buttons_down: HashSet<u8>,
}

impl Injector {
    pub fn new(target: TargetRect) -> Self {
        Self { target, keys_down: HashSet::new(), buttons_down: HashSet::new() }
    }

    /// Actualiza el monitor objetivo al cambiar de pantalla durante la sesión.
    pub fn set_target(&mut self, target: TargetRect) {
        self.target = target;
    }

    /// Procesa un mensaje de entrada del visor. Ignora cualquier otro tipo.
    pub fn handle(&mut self, msg: &Message) {
        match *msg {
            Message::MouseMove { x, y } => self.mouse_move(x, y),
            Message::MouseButton { button, down } => self.mouse_button(button, down),
            Message::MouseWheel { dx, dy } => self.mouse_wheel(dx, dy),
            Message::Key { scancode, extended, down } => self.key(scancode, extended, down),
            Message::Unicode { ch } => self.unicode(ch),
            Message::ReleaseAll => self.release_all(),
            _ => {}
        }
    }

    fn mouse_move(&self, x: u16, y: u16) {
        // Coordenada local del monitor (0..=65535) → píxel absoluto → normalizada al escritorio virtual,
        // que es el sistema que entiende MOUSEEVENTF_VIRTUALDESK.
        let (vx, vy, vw, vh) = virtual_desktop();
        if vw <= 1 || vh <= 1 {
            return;
        }
        let px = self.target.x as f64 + x as f64 / 65535.0 * self.target.width as f64;
        let py = self.target.y as f64 + y as f64 / 65535.0 * self.target.height as f64;
        let nx = ((px - vx as f64) / (vw - 1) as f64 * 65535.0).round().clamp(0.0, 65535.0) as i32;
        let ny = ((py - vy as f64) / (vh - 1) as f64 * 65535.0).round().clamp(0.0, 65535.0) as i32;
        send_mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK, nx, ny, 0);
    }

    fn mouse_button(&mut self, button: MouseButton, down: bool) {
        let (flag, data) = match (button, down) {
            (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
            (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
            (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
            (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
            (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
            (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
            // Los botones laterales usan XBUTTON1/XBUTTON2 en mouseData.
            (MouseButton::Back, true) => (MOUSEEVENTF_XDOWN, XBUTTON1),
            (MouseButton::Back, false) => (MOUSEEVENTF_XUP, XBUTTON1),
            (MouseButton::Forward, true) => (MOUSEEVENTF_XDOWN, XBUTTON2),
            (MouseButton::Forward, false) => (MOUSEEVENTF_XUP, XBUTTON2),
        };
        if down {
            self.buttons_down.insert(button as u8);
        } else {
            self.buttons_down.remove(&(button as u8));
        }
        send_mouse(flag, 0, 0, data);
    }

    fn mouse_wheel(&self, dx: i32, dy: i32) {
        if dy != 0 {
            send_mouse(MOUSEEVENTF_WHEEL, 0, 0, dy);
        }
        if dx != 0 {
            send_mouse(MOUSEEVENTF_HWHEEL, 0, 0, dx);
        }
    }

    fn key(&mut self, scancode: u16, extended: bool, down: bool) {
        if scancode == 0x5B || scancode == 0x5C {
            tracing::info!("inyectada tecla Windows (down={down}, ext={extended})");
        }
        if down {
            self.keys_down.insert(scancode | if extended { 0x8000 } else { 0 });
        } else {
            self.keys_down.remove(&(scancode | if extended { 0x8000 } else { 0 }));
        }
        send_key_scancode(scancode, extended, down);
    }

    fn unicode(&self, ch: u32) {
        // Los caracteres fuera del BMP se envían como par suplente (dos eventos UTF-16).
        if let Some(c) = char::from_u32(ch) {
            let mut buf = [0u16; 2];
            for unit in c.encode_utf16(&mut buf) {
                send_unicode(*unit, true);
                send_unicode(*unit, false);
            }
        }
    }

    /// Suelta todas las teclas y botones que quedaran pulsados.
    pub fn release_all(&mut self) {
        for key in std::mem::take(&mut self.keys_down) {
            send_key_scancode(key & 0x7FFF, key & 0x8000 != 0, false);
        }
        for button in std::mem::take(&mut self.buttons_down) {
            let (flag, data) = match button {
                b if b == MouseButton::Left as u8 => (MOUSEEVENTF_LEFTUP, 0),
                b if b == MouseButton::Right as u8 => (MOUSEEVENTF_RIGHTUP, 0),
                b if b == MouseButton::Middle as u8 => (MOUSEEVENTF_MIDDLEUP, 0),
                b if b == MouseButton::Back as u8 => (MOUSEEVENTF_XUP, XBUTTON1),
                _ => (MOUSEEVENTF_XUP, XBUTTON2),
            };
            send_mouse(flag, 0, 0, data);
        }
    }
}

impl Drop for Injector {
    fn drop(&mut self) {
        self.release_all();
    }
}

fn virtual_desktop() -> (i32, i32, i32, i32) {
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

fn send_mouse(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32, data: i32) {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: EXTRA_INFO,
            },
        },
    };
    unsafe { SendInput(&[input], size_of::<INPUT>() as i32) };
}

fn send_key_scancode(scancode: u16, extended: bool, down: bool) {
    let mut flags = KEYEVENTF_SCANCODE;
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VIRTUAL_KEY(0), wScan: scancode, dwFlags: flags, time: 0, dwExtraInfo: EXTRA_INFO },
        },
    };
    unsafe { SendInput(&[input], size_of::<INPUT>() as i32) };
}

fn send_unicode(unit: u16, down: bool) {
    let mut flags = KEYEVENTF_UNICODE;
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VIRTUAL_KEY(0), wScan: unit, dwFlags: flags, time: 0, dwExtraInfo: EXTRA_INFO },
        },
    };
    unsafe { SendInput(&[input], size_of::<INPUT>() as i32) };
}
