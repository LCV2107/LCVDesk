//! Ventana principal: estado del equipo, conexión a otro equipo, vista remota y control.

use crate::host::Host;
use crate::keymap;
use crate::uac;
use crate::updater::{self, Updater};
use crate::viewer::{Status, Viewer};
use eframe::egui::{
    self, Color32, ColorImage, Event, Key, Modifiers, PointerButton, Rect, RichText, Sense,
    TextureHandle, TextureOptions, pos2,
};
use eframe::egui_wgpu::SurfaceConfig;
use lcv_proto::{Message, MouseButton};
use std::time::Duration;

pub fn run(host: Host) -> anyhow::Result<()> {
    let mut options = eframe::NativeOptions::default();
    options.viewport = egui::ViewportBuilder::default()
        .with_title("LCVDesk")
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([520.0, 360.0]);
    // VSync con un solo fotograma en cola: baja latencia de presentación sin que la ventana se
    // redibuje de forma ilimitada (eso saturaba un núcleo y frenaba la captura DXGI del host).
    options.wgpu_options.surface = SurfaceConfig {
        present_mode: eframe::wgpu::PresentMode::AutoVsync,
        desired_maximum_frame_latency: Some(1),
    };
    eframe::run_native("LCVDesk", options, Box::new(|_cc| Ok(Box::new(App::new(host)))))
        .map_err(|e| anyhow::anyhow!("no se pudo abrir la ventana: {e}"))
}

struct App {
    host: Host,
    viewer: Option<Viewer>,
    address: String,
    texture: Option<TextureHandle>,
    local_name: String,
    local_ip: String,
    present_ms: f32,
    last_close_reason: Option<String>,
    uac_enabled: Option<bool>,
    updater: Updater,
    update_checked: bool,
    /// Estado de los modificadores enviado por última vez al host.
    sent_mods: Modifiers,
    /// Última posición del ratón enviada (para no repetir).
    last_mouse: Option<(u16, u16)>,
    /// El control remoto está activo (la vista tiene el foco).
    controlling: bool,
}

impl App {
    fn new(host: Host) -> Self {
        Self {
            host,
            viewer: None,
            address: String::new(),
            texture: None,
            local_name: hostname::get().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default(),
            local_ip: local_ip().unwrap_or_else(|| "sin red".into()),
            present_ms: 0.0,
            last_close_reason: None,
            uac_enabled: uac::is_enabled(),
            updater: Updater::new(),
            update_checked: false,
            sent_mods: Modifiers::default(),
            last_mouse: None,
            controlling: false,
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("LCVDesk").strong().size(16.0));
                ui.separator();
                let port = self.host.state().port;
                ui.label(format!("Este equipo: {}  ·  {}:{port}", self.local_name, self.local_ip));
            });
            let mut disconnect_host = false;
            {
                let st = self.host.state();
                if let Some(err) = &st.listen_error {
                    ui.colored_label(Color32::RED, format!("No se puede recibir conexiones: {err}"));
                }
                if let Some(s) = &st.session {
                    ui.horizontal(|ui| {
                        ui.colored_label(
                            Color32::from_rgb(220, 60, 60),
                            format!("● «{}» ({}) está viendo y controlando este equipo", s.name, s.peer.ip()),
                        );
                        disconnect_host = ui.button("Desconectar").clicked();
                    });
                }
            }
            if disconnect_host {
                self.host.disconnect();
            }
        });
    }

    fn home(&mut self, ui: &mut egui::Ui) {
        ui.add_space(20.0);
        ui.heading("Conectarse a otro equipo");
        ui.add_space(8.0);
        ui.label("Escribe la IP o el nombre del equipo (debe tener LCVDesk abierto).");
        ui.add_space(8.0);
        let mut connect = false;
        ui.horizontal(|ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.address).hint_text("192.168.0.20 o PC-TALLER").desired_width(260.0),
            );
            connect |= edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            connect |= ui.button("Conectar").clicked();
        });
        if connect && !self.address.trim().is_empty() {
            self.texture = None;
            self.last_close_reason = None;
            self.controlling = false;
            self.viewer = Some(Viewer::connect(self.address.trim().to_string(), ui.ctx().clone()));
        }
        if let Some(reason) = &self.last_close_reason {
            ui.add_space(8.0);
            ui.colored_label(Color32::from_rgb(230, 160, 40), reason);
        }

        ui.add_space(28.0);
        ui.separator();
        ui.add_space(8.0);
        self.updates_section(ui);

        ui.add_space(20.0);
        ui.separator();
        ui.add_space(8.0);
        self.uac_section(ui);
    }

    fn updates_section(&mut self, ui: &mut egui::Ui) {
        use updater::Status;
        ui.horizontal(|ui| {
            ui.heading("Actualizaciones");
            ui.label(RichText::new(format!("· versión instalada {}", updater::current_version())).color(Color32::GRAY));
        });
        ui.add_space(4.0);
        let busy = matches!(&*self.updater.status(), Status::Checking | Status::Downloading(_) | Status::Installing);
        ui.horizontal(|ui| {
            if ui.add_enabled(!busy, egui::Button::new("Buscar actualizaciones")).clicked() {
                self.updater.check(ui.ctx().clone());
            }
            // El botón de instalar solo aparece si hay una versión encontrada.
            let found = matches!(&*self.updater.status(), Status::Found(_));
            if found && ui.button("Descargar e instalar").clicked() {
                if let Status::Found(r) = &*self.updater.status() {
                    let release = r.clone();
                    self.updater.install(release, ui.ctx().clone());
                }
            }
        });
        ui.add_space(4.0);
        match &*self.updater.status() {
            Status::Idle => {}
            Status::Checking => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Buscando…");
                });
            }
            Status::UpToDate => {
                ui.colored_label(Color32::from_rgb(40, 160, 70), "Estás en la última versión.");
            }
            Status::Found(r) => {
                ui.colored_label(Color32::from_rgb(40, 110, 220), format!("Hay una versión nueva: {}", r.version));
                if !r.notes.is_empty() {
                    ui.label(RichText::new(truncate(&r.notes, 300)).small().color(Color32::GRAY));
                }
                if !r.page.is_empty() {
                    ui.hyperlink_to("Ver notas en GitHub", &r.page);
                }
            }
            Status::Downloading(pct) => {
                ui.add(egui::ProgressBar::new(*pct as f32 / 100.0).show_percentage().desired_width(360.0));
                ui.label(RichText::new("Descargando y verificando…").small().color(Color32::GRAY));
            }
            Status::Installing => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Instalando… LCVDesk se cerrará y volverá a abrirlo tras actualizar.");
                });
            }
            Status::Error(e) => {
                ui.colored_label(Color32::from_rgb(210, 90, 60), format!("No se pudo actualizar: {e}"));
            }
        }
    }

    fn uac_section(&mut self, ui: &mut egui::Ui) {
        ui.heading("Permitir ventanas de administrador (UAC) en remoto");
        ui.add_space(4.0);
        match self.uac_enabled {
            None => {
                ui.colored_label(Color32::GRAY, "No se pudo leer el estado (permisos insuficientes).");
            }
            Some(state) => {
                let mut on = state;
                let resp = ui.checkbox(
                    &mut on,
                    "Mostrar los avisos de UAC en el escritorio normal (para poder aceptarlos en remoto)",
                );
                ui.label(
                    RichText::new(
                        "Al activarlo, los permisos de administrador dejan de atenuar la pantalla y se pueden \
                         controlar en remoto. Reduce una protección de Windows; desactívalo cuando no lo \
                         necesites. No afecta a la pantalla de bloqueo. Pedirá confirmación de administrador.",
                    )
                    .small()
                    .color(Color32::from_rgb(200, 170, 90)),
                );
                if resp.changed() {
                    if uac::set(on) {
                        // Releer el valor real tras la elevación (el usuario pudo cancelar).
                        self.uac_enabled = uac::is_enabled();
                    } else {
                        self.uac_enabled = uac::is_enabled();
                    }
                }
            }
        }
    }

    fn remote(&mut self, ui: &mut egui::Ui) {
        let Some(viewer) = self.viewer.as_ref() else { return };
        let (status_text, stats, frame, closed, displays, current) = {
            let mut sh = viewer.shared();
            let text = match &sh.status {
                Status::Connecting => Some(format!("Conectando con {}…", viewer.target)),
                Status::WaitingAcceptance => Some("Esperando a que el otro usuario acepte…".to_string()),
                Status::Streaming => None,
                Status::Closed(r) => Some(r.clone()),
            };
            let closed = matches!(sh.status, Status::Closed(_));
            (text, sh.stats, sh.frame.take(), closed, sh.displays.clone(), sh.current_display)
        };

        if closed {
            self.last_close_reason = status_text;
            self.viewer = None;
            self.controlling = false;
            return;
        }

        if let Some(f) = frame {
            let image = ColorImage::from_rgba_unmultiplied([f.width as usize, f.height as usize], &f.pixels);
            match &mut self.texture {
                Some(t) => t.set(image, TextureOptions::LINEAR),
                None => self.texture = Some(ui.ctx().load_texture("remote", image, TextureOptions::LINEAR)),
            }
            self.present_ms = f.received.elapsed().as_secs_f32() * 1000.0;
        }

        ui.horizontal(|ui| {
            let viewer = self.viewer.as_ref().unwrap();
            if ui.button("Desconectar").clicked() {
                viewer.disconnect();
            }
            // Selector de monitor (solo si el equipo remoto tiene más de uno).
            if displays.len() > 1 {
                ui.separator();
                ui.label("Monitor:");
                let label = displays
                    .iter()
                    .find(|d| d.index == current)
                    .map_or_else(|| current.to_string(), |d| display_label(d));
                egui::ComboBox::from_id_salt("monitor").selected_text(label).show_ui(ui, |ui| {
                    for d in &displays {
                        if ui.selectable_label(d.index == current, display_label(d)).clicked() && d.index != current {
                            viewer.select_display(d.index);
                        }
                    }
                });
            }
            ui.separator();
            match &status_text {
                Some(t) => {
                    ui.label(t);
                }
                None => {
                    ui.monospace(format!(
                        "{}x{} {:.0}fps {:.1}Mbps | red {:.1}ms host {:.1}ms decod {:.1}ms pres {:.1}ms",
                        stats.width, stats.height, stats.fps, stats.mbps, stats.rtt_ms, stats.host_ms, stats.decode_ms, self.present_ms
                    ));
                }
            };
        });
        if status_text.is_none() {
            ui.label(
                RichText::new(if self.controlling {
                    "Controlando: tu ratón y teclado van al equipo remoto. Haz clic fuera de la imagen para soltar el control."
                } else {
                    "Haz clic en la imagen para controlar el equipo remoto."
                })
                .small()
                .color(Color32::GRAY),
            );
        }

        let area = ui.available_rect_before_wrap();
        ui.painter().rect_filled(area, 0.0, Color32::BLACK);
        let image_rect = self.texture.as_ref().map(|tex| {
            let size = tex.size_vec2();
            let scale = (area.width() / size.x).min(area.height() / size.y);
            let rect = Rect::from_center_size(area.center(), size * scale);
            ui.painter().image(tex.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            rect
        });

        let response = ui.allocate_rect(area, Sense::click_and_drag());
        if status_text.is_none() {
            self.handle_control(ui, &response, image_rect);
        }
    }

    /// Reenvía ratón y teclado al host cuando la vista remota tiene el control.
    fn handle_control(&mut self, ui: &egui::Ui, response: &egui::Response, image_rect: Option<Rect>) {
        let Some(viewer) = self.viewer.as_ref() else { return };
        let Some(rect) = image_rect else { return };

        if response.clicked() {
            response.request_focus();
            self.controlling = true;
        }
        if response.clicked_elsewhere() {
            if self.controlling {
                viewer.send(&Message::ReleaseAll);
                self.sent_mods = Modifiers::default();
            }
            self.controlling = false;
        }
        if !self.controlling || !response.has_focus() {
            if self.controlling {
                viewer.send(&Message::ReleaseAll);
                self.sent_mods = Modifiers::default();
                self.controlling = false;
            }
            return;
        }

        // --- Ratón: posición normalizada 0..=65535 sobre la imagen remota. ---
        if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
            if rect.contains(pos) {
                let nx = ((pos.x - rect.min.x) / rect.width() * 65535.0).clamp(0.0, 65535.0) as u16;
                let ny = ((pos.y - rect.min.y) / rect.height() * 65535.0).clamp(0.0, 65535.0) as u16;
                if self.last_mouse != Some((nx, ny)) {
                    viewer.send(&Message::MouseMove { x: nx, y: ny });
                    self.last_mouse = Some((nx, ny));
                }
            }
        }

        // --- Eventos (botones, rueda, teclado). ---
        let (events, mods) = ui.input(|i| (i.events.clone(), i.modifiers));
        // Modificadores: enviar los cambios de Ctrl/Shift/Alt/Win como pulsaciones de tecla.
        // Se escribe en self.sent_mods mientras `viewer` sigue prestado: son campos distintos.
        {
            let prev = self.sent_mods;
            let send_mod = |vk: u16, down: bool| {
                if let Some((sc, ext)) = keymap::vk_to_scancode(vk) {
                    viewer.send(&Message::Key { scancode: sc, extended: ext, down });
                }
            };
            if mods.ctrl != prev.ctrl {
                send_mod(keymap::VK_CONTROL, mods.ctrl);
            }
            if mods.shift != prev.shift {
                send_mod(keymap::VK_SHIFT, mods.shift);
            }
            if mods.alt != prev.alt {
                send_mod(keymap::VK_MENU, mods.alt);
            }
            if mods.command != prev.command {
                send_mod(keymap::VK_LWIN, mods.command);
            }
            self.sent_mods = mods;
        }
        for event in &events {
            match event {
                Event::PointerButton { button, pressed, pos, .. } if rect.contains(*pos) => {
                    if let Some(b) = map_button(*button) {
                        viewer.send(&Message::MouseButton { button: b, down: *pressed });
                    }
                }
                Event::MouseWheel { delta, .. } => {
                    // Una muesca de rueda = 120; el delta de egui viene en líneas/puntos.
                    let dy = (delta.y * 120.0).round() as i32;
                    let dx = (delta.x * 120.0).round() as i32;
                    if dx != 0 || dy != 0 {
                        viewer.send(&Message::MouseWheel { dx, dy });
                    }
                }
                Event::Text(text) if !mods.ctrl && !mods.alt && !mods.command => {
                    for ch in text.chars() {
                        viewer.send(&Message::Unicode { ch: ch as u32 });
                    }
                }
                Event::Key { key, pressed, .. } => {
                    // El texto imprimible sin modificadores ya se envía como Unicode; evita duplicar.
                    let is_shortcut = mods.ctrl || mods.alt || mods.command;
                    if keymap::is_text_key(*key) && !is_shortcut {
                        continue;
                    }
                    if let Some(vk) = keymap::egui_key_to_vk(*key) {
                        if let Some((sc, ext)) = keymap::vk_to_scancode(vk) {
                            viewer.send(&Message::Key { scancode: sc, extended: ext, down: *pressed });
                        }
                    }
                }
                _ => {}
            }
        }
    }

}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.ctx().request_repaint_after(Duration::from_millis(500));
        // Comprobación automática de actualizaciones, una vez al arrancar.
        if !self.update_checked {
            self.update_checked = true;
            self.updater.check(ui.ctx().clone());
        }
        self.top_bar(ui);
        egui::CentralPanel::default().show(ui, |ui| {
            if self.viewer.is_some() { self.remote(ui) } else { self.home(ui) }
        });
    }
}

fn display_label(d: &lcv_proto::DisplayInfo) -> String {
    let n = d.name.rsplit('\\').next().unwrap_or(&d.name);
    format!("{n} ({}x{}){}", d.width, d.height, if d.primary { " ★" } else { "" })
}

fn map_button(b: PointerButton) -> Option<MouseButton> {
    match b {
        PointerButton::Primary => Some(MouseButton::Left),
        PointerButton::Secondary => Some(MouseButton::Right),
        PointerButton::Middle => Some(MouseButton::Middle),
        _ => None,
    }
}

/// Recorta un texto largo (notas de la versión) para mostrarlo en la interfaz.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

/// IP local de la interfaz con ruta por defecto (no envía paquetes).
fn local_ip() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    Some(socket.local_addr().ok()?.ip().to_string())
}
