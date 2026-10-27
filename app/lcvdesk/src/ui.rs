//! Ventana principal: estado del equipo, conexión a otro equipo, vista remota y control.

use crate::discovery::Discovery;
use crate::host::Host;
use crate::keyhook;
use crate::keymap;
use crate::recents::{self, SavedDevice};
use crate::uac;
use crate::updater::{self, Updater};
use crate::viewer::{Status, Viewer};
use eframe::egui::{
    self, Color32, ColorImage, CornerRadius, Event, Frame, Key, Modifiers, PointerButton, Rect,
    RichText, Sense, Stroke, TextureHandle, TextureOptions, ThemePreference, pos2,
};
use eframe::egui_wgpu::SurfaceConfig;
use lcv_proto::{Message, MouseButton};
use std::time::Duration;

/// Preferencia de tema de la interfaz.
#[derive(Clone, Copy, PartialEq)]
enum Theme {
    System,
    Light,
    Dark,
}

impl Theme {
    fn from_str(s: &str) -> Self {
        match s {
            "light" => Theme::Light,
            "dark" => Theme::Dark,
            _ => Theme::System,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }
    fn preference(self) -> ThemePreference {
        match self {
            Theme::System => ThemePreference::System,
            Theme::Light => ThemePreference::Light,
            Theme::Dark => ThemePreference::Dark,
        }
    }
}

/// Canal de actualizaciones.
#[derive(Clone, Copy, PartialEq)]
enum Channel {
    Stable,
    Beta,
}

impl Channel {
    fn from_str(s: &str) -> Self {
        if s == "beta" { Channel::Beta } else { Channel::Stable }
    }
    fn as_str(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Beta => "beta",
        }
    }
    fn is_beta(self) -> bool {
        matches!(self, Channel::Beta)
    }
}

/// Verde "conectado", gris "desconectado", naranja "ocupado", rojo "controlando".
const GREEN: Color32 = Color32::from_rgb(46, 170, 90);
const ORANGE: Color32 = Color32::from_rgb(210, 150, 50);
const RED: Color32 = Color32::from_rgb(214, 78, 70);

pub fn run(host: Host, discovery: Discovery) -> anyhow::Result<()> {
    let mut options = eframe::NativeOptions::default();
    options.viewport = egui::ViewportBuilder::default()
        .with_title("LCVDesk")
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([520.0, 360.0]);
    // Icono de la ventana (barra de tareas y Alt+Tab).
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")) {
        options.viewport = options.viewport.with_icon(icon);
    }
    // VSync con un solo fotograma en cola: baja latencia de presentación sin que la ventana se
    // redibuje de forma ilimitada (eso saturaba un núcleo y frenaba la captura DXGI del host).
    options.wgpu_options.surface = SurfaceConfig {
        present_mode: eframe::wgpu::PresentMode::AutoVsync,
        desired_maximum_frame_latency: Some(1),
    };
    eframe::run_native("LCVDesk", options, Box::new(|_cc| Ok(Box::new(App::new(host, discovery)))))
        .map_err(|e| anyhow::anyhow!("no se pudo abrir la ventana: {e}"))
}

struct App {
    host: Host,
    discovery: Discovery,
    /// Equipos recientes (se recarga del disco al conectar o al cambiar favoritos).
    recents: Vec<SavedDevice>,
    /// Evita guardar el mismo "reciente" varias veces durante una sesión.
    recorded_recent: bool,
    viewer: Option<Viewer>,
    address: String,
    /// Contraseña que el usuario escribe en el cuadro que aparece al conectar.
    prompt_password: String,
    /// Marcar "recordar contraseña" en ese cuadro.
    prompt_remember: bool,
    /// Campo para configurar la contraseña de acceso de ESTE equipo (lado controlado).
    access_password: String,
    /// ¿Hay una contraseña de acceso configurada en este equipo?
    has_access_password: bool,
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
    /// Captura de teclado activa: todas las teclas (Windows, Alt+Tab…) van al equipo remoto.
    capture: bool,
    /// Tema de la interfaz (persistido).
    theme: Theme,
    /// Canal de actualizaciones (persistido).
    channel: Channel,
    /// El usuario ocultó el aviso de actualización (no crítica) en esta sesión.
    update_dismissed: bool,
}

impl App {
    fn new(host: Host, discovery: Discovery) -> Self {
        Self {
            host,
            discovery,
            recents: recents::load(),
            recorded_recent: false,
            viewer: None,
            address: String::new(),
            prompt_password: String::new(),
            prompt_remember: false,
            access_password: String::new(),
            has_access_password: crate::auth::has_password(),
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
            capture: true,
            theme: Theme::from_str(&crate::settings::load_theme()),
            channel: Channel::from_str(&crate::settings::load_channel()),
            update_dismissed: false,
        }
    }

    /// Devuelve la versión nueva encontrada (si la hay), para avisos y actualización obligatoria.
    fn found_release(&self) -> Option<updater::Release> {
        match &*self.updater.status() {
            updater::Status::Found(r) => Some(r.clone()),
            _ => None,
        }
    }

    /// Aplica el tema elegido y el acento azul, en claro u oscuro con el mismo diseño.
    fn apply_theme(&self, ctx: &egui::Context) {
        ctx.set_theme(self.theme.preference());
        // El mismo acento y redondeos en claro y oscuro (comparten diseño).
        ctx.style_mut_of(egui::Theme::Dark, |s| tune_style(s, true));
        ctx.style_mut_of(egui::Theme::Light, |s| tune_style(s, false));
    }

    fn accent(ui: &egui::Ui) -> Color32 {
        if ui.visuals().dark_mode { Color32::from_rgb(74, 144, 246) } else { Color32::from_rgb(26, 108, 224) }
    }

    /// Franja superior: solo aparece si hay un aviso (error de escucha, alguien controlándote o
    /// una actualización obligatoria pendiente mientras hay sesión).
    fn status_strip(&mut self, ui: &mut egui::Ui) {
        let (listen_error, session) = {
            let st = self.host.state();
            (st.listen_error.clone(), st.session.as_ref().map(|s| (s.name.clone(), s.peer.ip())))
        };
        let critical_in_session = self.viewer.is_some() && self.found_release().is_some_and(|r| r.critical);
        if listen_error.is_none() && session.is_none() && !critical_in_session {
            return;
        }
        egui::Panel::top("status").show(ui, |ui| {
            ui.add_space(4.0);
            if let Some(err) = &listen_error {
                ui.colored_label(RED, format!("⚠ No se pueden recibir conexiones: {err}"));
            }
            if let Some((name, ip)) = &session {
                ui.horizontal(|ui| {
                    ui.colored_label(RED, "●");
                    ui.label(format!("«{name}» ({ip}) está viendo y controlando este equipo"));
                    if ui.button("Desconectar").clicked() {
                        self.host.disconnect();
                    }
                });
            }
            if critical_in_session {
                ui.colored_label(RED, "⚠ Actualización obligatoria disponible: se aplicará al cerrar la sesión.");
            }
            ui.add_space(4.0);
        });
    }

    /// Barra lateral: identidad de este equipo y ajustes.
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.label(RichText::new("LCVDesk").strong().size(22.0).color(Self::accent(ui)));
        ui.label(RichText::new(format!("v{}", updater::current_version())).small().color(Color32::GRAY));
        ui.add_space(14.0);

        ui.label(RichText::new("ESTE EQUIPO").small().strong().color(Color32::GRAY));
        ui.add_space(4.0);
        let port = self.host.state().port;
        Frame::group(ui.style()).corner_radius(10).inner_margin(12).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(&self.local_name).strong().size(16.0));
            ui.add_space(2.0);
            ui.label(RichText::new(format!("{}:{port}", self.local_ip)).monospace());
            ui.add_space(8.0);
            let mut visible = self.discovery.is_discoverable();
            if ui.checkbox(&mut visible, "Visible en la red").changed() {
                self.discovery.set_discoverable(visible);
            }
        });

        ui.add_space(14.0);
        egui::CollapsingHeader::new(RichText::new("⚙  Ajustes").strong()).default_open(false).show(ui, |ui| {
            ui.add_space(4.0);
            ui.label(RichText::new("Tema").small().strong().color(Color32::GRAY));
            ui.horizontal(|ui| {
                for (label, t) in [("Sistema", Theme::System), ("Claro", Theme::Light), ("Oscuro", Theme::Dark)] {
                    if ui.selectable_label(self.theme == t, label).clicked() {
                        self.theme = t;
                        crate::settings::save_theme(t.as_str());
                    }
                }
            });
            ui.add_space(10.0);

            ui.label(RichText::new("Canal de actualizaciones").small().strong().color(Color32::GRAY));
            ui.horizontal(|ui| {
                for (label, c) in [("Estable", Channel::Stable), ("Beta", Channel::Beta)] {
                    if ui.selectable_label(self.channel == c, label).clicked() && self.channel != c {
                        self.channel = c;
                        crate::settings::save_channel(c.as_str());
                        self.update_dismissed = false;
                        self.updater.check(ui.ctx().clone(), self.channel.is_beta());
                    }
                }
            });
            if self.channel.is_beta() {
                ui.label(
                    RichText::new("Recibes versiones de prueba antes que el resto. Pueden ser inestables.")
                        .small()
                        .color(ORANGE),
                );
            }
            ui.add_space(10.0);
            self.access_password_section(ui);
            ui.add_space(10.0);
            self.uac_section(ui);
            ui.add_space(10.0);
            self.updates_section(ui);
        });
    }

    /// Área principal cuando no hay sesión: barra de conexión y tarjetas de equipos.
    fn main_area(&mut self, ui: &mut egui::Ui) {
        self.update_banner(ui);
        ui.add_space(16.0);
        ui.heading("Conectar a un equipo");
        ui.add_space(8.0);
        let mut connect = false;
        ui.horizontal(|ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.address)
                    .hint_text("IP o nombre del equipo")
                    .desired_width(300.0),
            );
            connect |= edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if ui.add(egui::Button::new(RichText::new("Conectar").strong())).clicked() {
                connect = true;
            }
        });
        ui.label(
            RichText::new("Si el equipo tiene contraseña, se te pedirá al conectar.")
                .small()
                .color(Color32::GRAY),
        );
        if connect {
            let addr = self.address.clone();
            self.start_connect(addr.clone(), addr, None, ui.ctx());
        }
        if let Some(reason) = &self.last_close_reason {
            ui.add_space(6.0);
            ui.colored_label(ORANGE, reason);
        }

        ui.add_space(20.0);
        self.devices_section(ui);
    }

    /// Aviso discreto (no crítico) de actualización disponible, en la parte superior.
    fn update_banner(&mut self, ui: &mut egui::Ui) {
        if self.update_dismissed {
            return;
        }
        let Some(r) = self.found_release() else { return };
        if r.critical {
            return; // la crítica usa la pantalla obligatoria
        }
        let accent = Self::accent(ui);
        let tint = accent.gamma_multiply(if ui.visuals().dark_mode { 0.22 } else { 0.12 });
        ui.add_space(10.0);
        Frame::group(ui.style()).fill(tint).stroke(Stroke::new(1.0, accent)).corner_radius(10).inner_margin(10).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(accent, "●");
                ui.label(RichText::new(format!("Actualización disponible: {}", r.version)).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Ahora no").clicked() {
                        self.update_dismissed = true;
                    }
                    if ui.button(RichText::new("Instalar").strong()).clicked() {
                        self.updater.install(r.clone(), ui.ctx().clone());
                    }
                });
            });
        });
    }

    /// Pantalla a pantalla completa para una actualización crítica: solo se puede actualizar o salir.
    fn mandatory_update(&mut self, ui: &mut egui::Ui, r: updater::Release) {
        use updater::Status;
        // Instantánea del estado para no mantener bloqueado el mutex mientras se dibuja.
        let phase = match &*self.updater.status() {
            Status::Downloading(p) => Status::Downloading(*p),
            Status::Installing => Status::Installing,
            Status::Error(e) => Status::Error(e.clone()),
            _ => Status::Idle,
        };
        ui.add_space(48.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("⚠ Actualización obligatoria").size(26.0).strong().color(RED));
            ui.add_space(8.0);
            ui.label("Esta versión incluye un cambio crítico. Debes actualizar para seguir usando LCVDesk.");
            ui.add_space(6.0);
            ui.label(RichText::new(format!("Nueva versión: {}", r.version)).strong());
            if !r.notes.is_empty() {
                ui.add_space(4.0);
                ui.label(RichText::new(truncate(&r.notes, 300)).small().color(Color32::GRAY));
            }
            ui.add_space(18.0);
            match phase {
                Status::Downloading(pct) => {
                    ui.add(egui::ProgressBar::new(pct as f32 / 100.0).show_percentage().desired_width(280.0));
                    ui.label(RichText::new("Descargando y verificando…").small().color(Color32::GRAY));
                }
                Status::Installing => {
                    ui.spinner();
                    ui.label("Instalando… LCVDesk se cerrará para aplicar la actualización.");
                }
                _ => {
                    if let Status::Error(e) = &phase {
                        ui.colored_label(RED, format!("No se pudo actualizar: {e}"));
                        ui.add_space(6.0);
                    }
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new("Descargar e instalar").strong()).clicked() {
                            self.updater.install(r.clone(), ui.ctx().clone());
                        }
                        if ui.button("Salir").clicked() {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                }
            }
        });
    }

    /// Inicia una conexión a un equipo. Si hay contraseña guardada para ese `id`, entra automáticamente;
    /// si no y el equipo la pide, el visor la solicitará por pantalla.
    fn start_connect(&mut self, addr: String, name: String, id: Option<String>, ctx: &egui::Context) {
        if addr.trim().is_empty() {
            return;
        }
        self.texture = None;
        self.last_close_reason = None;
        self.controlling = false;
        self.recorded_recent = false;
        self.prompt_password.clear();
        self.prompt_remember = false;
        let initial = id.as_deref().and_then(crate::auth::recall_password);
        let display = if name.trim().is_empty() { addr.trim().to_string() } else { name };
        self.viewer = Some(Viewer::connect(addr.trim().to_string(), display, initial, ctx.clone()));
    }

    /// Tarjetas de equipos encontrados en la red y de equipos recientes.
    fn devices_section(&mut self, ui: &mut egui::Ui) {
        let found = self.discovery.list();

        section_header(ui, "En la red");
        ui.add_space(6.0);
        if found.is_empty() {
            ui.label(RichText::new("Buscando equipos con LCVDesk en tu red…").small().color(Color32::GRAY));
        } else {
            let mut connect = None;
            ui.horizontal_wrapped(|ui| {
                for f in &found {
                    let act = device_card(ui, &f.name, &f.addr, true, f.busy, None);
                    if act.connect {
                        connect = Some((f.addr.clone(), f.name.clone(), Some(f.id.clone())));
                    }
                }
            });
            if let Some((a, n, id)) = connect {
                self.start_connect(a, n, id, ui.ctx());
            }
        }

        ui.add_space(18.0);
        section_header(ui, "Recientes");
        ui.add_space(6.0);
        if self.recents.is_empty() {
            ui.label(RichText::new("Aún no te has conectado a ningún equipo.").small().color(Color32::GRAY));
            return;
        }
        let online: std::collections::HashSet<&str> = found.iter().map(|f| f.id.as_str()).collect();
        let (mut connect, mut remove_id, mut fav_toggle) = (None, None, None);
        ui.horizontal_wrapped(|ui| {
            for d in &self.recents {
                let on = online.contains(d.id.as_str());
                let act = device_card(ui, &d.name, &d.addr, on, false, Some(d.favorite));
                if act.connect {
                    connect = Some((d.addr.clone(), d.name.clone(), Some(d.id.clone())));
                }
                if act.favorite {
                    fav_toggle = Some((d.id.clone(), !d.favorite));
                }
                if act.remove {
                    remove_id = Some(d.id.clone());
                }
            }
        });
        if let Some((id, fav)) = fav_toggle {
            recents::set_favorite(&id, fav);
            self.recents = recents::load();
        }
        if let Some(id) = remove_id {
            crate::auth::forget_password(&id);
            recents::remove(&id);
            self.recents = recents::load();
        }
        if let Some((a, n, id)) = connect {
            self.start_connect(a, n, id, ui.ctx());
        }
    }

    /// Configura la contraseña de acceso de ESTE equipo (lado controlado). Con contraseña, quien la
    /// sepa entra sin que haya que aceptar manualmente; sin ella (o si falla) se pide aceptación.
    fn access_password_section(&mut self, ui: &mut egui::Ui) {
        section_header(ui, "Acceso por contraseña");
        ui.add_space(4.0);
        if self.has_access_password {
            ui.horizontal(|ui| {
                ui.colored_label(Color32::from_rgb(40, 160, 70), "● Hay una contraseña configurada.");
                if ui.button("Quitar contraseña").clicked() {
                    crate::auth::clear_password();
                    self.has_access_password = crate::auth::has_password();
                    self.access_password.clear();
                }
            });
            ui.label(
                RichText::new(
                    "Quien conozca la contraseña podrá conectarse sin que tengas que aceptar. \
                     Si no la introduce o es incorrecta, aún aparecerá la ventana para aceptar.",
                )
                .small()
                .color(Color32::GRAY),
            );
        } else {
            ui.label(
                RichText::new(
                    "Sin contraseña, cada conexión te pide aceptar manualmente. Define una para \
                     permitir el acceso sin confirmación en este equipo.",
                )
                .small()
                .color(Color32::GRAY),
            );
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.access_password)
                        .password(true)
                        .hint_text("nueva contraseña")
                        .desired_width(180.0),
                );
                let can_save = !self.access_password.trim().is_empty();
                if ui.add_enabled(can_save, egui::Button::new("Guardar")).clicked() {
                    if crate::auth::save_password(&self.access_password) {
                        self.access_password.clear();
                        self.has_access_password = crate::auth::has_password();
                    }
                }
            });
        }
    }

    fn updates_section(&mut self, ui: &mut egui::Ui) {
        use updater::Status;
        section_header(ui, "Actualizaciones");
        ui.add_space(4.0);
        let busy = matches!(&*self.updater.status(), Status::Checking | Status::Downloading(_) | Status::Installing);
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(!busy, egui::Button::new("Buscar actualizaciones")).clicked() {
                self.updater.check(ui.ctx().clone(), self.channel.is_beta());
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
                ui.add(egui::ProgressBar::new(*pct as f32 / 100.0).show_percentage());
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
        section_header(ui, "UAC en remoto");
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
        let target = viewer.target.clone();
        let name = viewer.display_name.clone();
        let (status_text, stats, frame, closed, need_password, displays, current, host_id, host_name) = {
            let mut sh = viewer.shared();
            let text = match &sh.status {
                Status::Connecting => Some(format!("Conectando con {}…", viewer.target)),
                Status::NeedPassword => None,
                Status::WaitingAcceptance => Some("Esperando a que el otro usuario acepte…".to_string()),
                Status::Streaming => None,
                Status::Closed(r) => Some(r.clone()),
            };
            let closed = matches!(sh.status, Status::Closed(_));
            let need_password = matches!(sh.status, Status::NeedPassword);
            (text, sh.stats, sh.frame.take(), closed, need_password, sh.displays.clone(), sh.current_display, sh.host_id.clone(), sh.host_name.clone())
        };

        if closed {
            self.last_close_reason = status_text;
            self.viewer = None;
            self.controlling = false;
            keyhook::set_grab(false);
            return;
        }

        if need_password {
            self.password_prompt(ui, &name);
            return;
        }

        // Al confirmarse la sesión, guardar (una vez) el equipo en "Recientes".
        if status_text.is_none() && !self.recorded_recent && !host_id.is_empty() {
            recents::record(&host_id, &host_name, &target);
            self.recents = recents::load();
            self.recorded_recent = true;
        }

        if let Some(f) = frame {
            let image = ColorImage::from_rgba_unmultiplied([f.width as usize, f.height as usize], &f.pixels);
            match &mut self.texture {
                Some(t) => t.set(image, TextureOptions::LINEAR),
                None => self.texture = Some(ui.ctx().load_texture("remote", image, TextureOptions::LINEAR)),
            }
            self.present_ms = f.received.elapsed().as_secs_f32() * 1000.0;
        }

        let mut toggle_capture = false;
        ui.horizontal(|ui| {
            let viewer = self.viewer.as_ref().unwrap();
            if ui.button("Desconectar").clicked() {
                viewer.disconnect();
            }
            // Captura de teclado: cuando está activa, la tecla Windows, Alt+Tab, etc. van al remoto.
            if status_text.is_none() {
                ui.separator();
                let (txt, col) = if self.capture {
                    ("⌨ Teclado capturado", GREEN)
                } else {
                    ("⌨ Capturar teclado", Color32::GRAY)
                };
                if ui.button(RichText::new(txt).color(col)).on_hover_text("Soltar/recuperar con Ctrl+Alt+Inicio").clicked() {
                    toggle_capture = true;
                }
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
        if toggle_capture {
            self.capture = !self.capture;
            if !self.capture {
                if let Some(v) = self.viewer.as_ref() {
                    v.send(&Message::ReleaseAll);
                }
                self.sent_mods = Modifiers::default();
            }
        }
        if status_text.is_none() {
            let msg = if self.capture {
                "Teclado capturado: la tecla Windows, Alt+Tab, etc. van al equipo remoto. \
                 Pulsa Ctrl+Alt+Inicio para soltar el teclado y volver a tu equipo."
            } else {
                "Teclado NO capturado: tus teclas actúan en tu equipo. Pulsa «Capturar teclado» \
                 (o Ctrl+Alt+Inicio) para enviarlas al remoto. El ratón sigue controlando al pasar por la imagen."
            };
            ui.label(RichText::new(msg).small().color(if self.capture { GREEN } else { ORANGE }));
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
        } else {
            keyhook::set_grab(false);
        }
    }

    /// Cuadro para introducir la contraseña del equipo al que te conectas (aparece cada vez, salvo
    /// que esté guardada). Marcar "Recordar" la guarda para entrar automáticamente la próxima vez.
    fn password_prompt(&mut self, ui: &mut egui::Ui, name: &str) {
        ui.add_space(48.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("🔒 Contraseña requerida").size(22.0).strong());
            ui.add_space(6.0);
            ui.label(format!("El equipo «{name}» pide contraseña para conectar."));
            ui.add_space(10.0);
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.prompt_password)
                    .password(true)
                    .hint_text("contraseña")
                    .desired_width(240.0),
            );
            resp.request_focus();
            let submit = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            ui.add_space(6.0);
            ui.checkbox(&mut self.prompt_remember, "Recordar contraseña (entrar automáticamente)");
            ui.add_space(10.0);
            let (mut go, mut cancel) = (false, false);
            ui.horizontal(|ui| {
                go = ui.button(RichText::new("Conectar").strong()).clicked();
                cancel = ui.button("Cancelar").clicked();
            });
            if go || submit {
                if let Some(v) = self.viewer.as_ref() {
                    v.provide_password(self.prompt_password.clone(), self.prompt_remember);
                }
                self.prompt_password.clear();
            }
            if cancel {
                if let Some(v) = self.viewer.as_ref() {
                    v.cancel_password();
                }
            }
        });
    }

    /// Reenvía ratón y teclado al host. El teclado se captura en "modo captura" (todas las teclas,
    /// incluida Windows/Alt+Tab, van al remoto); el hook solo suprime cuando la ventana es nuestra.
    /// El ratón se reenvía siempre que el puntero esté sobre la imagen. Ctrl+Alt+Inicio suelta.
    fn handle_control(&mut self, ui: &egui::Ui, response: &egui::Response, image_rect: Option<Rect>) {
        let _ = response;
        let Some(viewer) = self.viewer.as_ref() else { return };
        let Some(rect) = image_rect else {
            keyhook::set_grab(false);
            return;
        };

        // Ctrl+Alt+Inicio alterna la captura del teclado.
        if keyhook::take_release_request() {
            self.capture = !self.capture;
            if !self.capture {
                viewer.send(&Message::ReleaseAll);
                self.sent_mods = Modifiers::default();
            }
        }

        let use_hook = keyhook::installed();
        keyhook::set_grab(self.capture && use_hook);
        self.controlling = self.capture;

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
        // Respaldo de teclado por eventos de egui solo si no hay hook y el modo captura está activo.
        let kbd_fallback = !use_hook && self.capture;
        // Modificadores (solo en el respaldo sin hook): enviar los cambios de Ctrl/Shift/Alt como
        // pulsaciones de tecla. Se escribe en self.sent_mods mientras `viewer` sigue prestado.
        if kbd_fallback {
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
                // El teclado con hook se reenvía por scancode desde keyhook; aquí solo el respaldo.
                Event::Text(text) if kbd_fallback && !mods.ctrl && !mods.alt && !mods.command => {
                    for ch in text.chars() {
                        viewer.send(&Message::Unicode { ch: ch as u32 });
                    }
                }
                Event::Key { key, pressed, .. } if kbd_fallback => {
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
        self.apply_theme(ui.ctx());
        // Comprobación automática de actualizaciones, una vez al arrancar.
        if !self.update_checked {
            self.update_checked = true;
            self.updater.check(ui.ctx().clone(), self.channel.is_beta());
        }
        let critical = self.found_release().filter(|r| r.critical);
        self.status_strip(ui);
        // Actualización crítica: se obliga a instalar (fuera de una sesión activa, para no cortarla).
        if self.viewer.is_none() {
            if let Some(r) = critical {
                egui::CentralPanel::default().show(ui, |ui| self.mandatory_update(ui, r));
                return;
            }
        }
        if self.viewer.is_some() {
            egui::CentralPanel::default().show(ui, |ui| self.remote(ui));
        } else {
            egui::Panel::left("side").resizable(false).show(ui, |ui| {
                ui.set_min_width(248.0);
                egui::ScrollArea::vertical().show(ui, |ui| self.sidebar(ui));
            });
            egui::CentralPanel::default().show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.main_area(ui));
            });
        }
    }
}

/// Ajusta un estilo (acento azul y redondeos) para el tema claro u oscuro.
fn tune_style(s: &mut egui::Style, dark: bool) {
    let accent = if dark { Color32::from_rgb(74, 144, 246) } else { Color32::from_rgb(26, 108, 224) };
    s.visuals.selection.bg_fill = accent.gamma_multiply(if dark { 0.45 } else { 0.30 });
    s.visuals.selection.stroke = Stroke::new(1.0, accent);
    s.visuals.hyperlink_color = accent;
    s.visuals.widgets.hovered.weak_bg_fill = accent.gamma_multiply(if dark { 0.30 } else { 0.18 });
    s.visuals.widgets.active.weak_bg_fill = accent;
    s.visuals.widgets.active.bg_fill = accent;
    let cr = CornerRadius::same(7);
    s.visuals.widgets.noninteractive.corner_radius = cr;
    s.visuals.widgets.inactive.corner_radius = cr;
    s.visuals.widgets.hovered.corner_radius = cr;
    s.visuals.widgets.active.corner_radius = cr;
    s.visuals.widgets.open.corner_radius = cr;
    s.visuals.window_corner_radius = CornerRadius::same(10);
    s.spacing.item_spacing = egui::vec2(8.0, 8.0);
    s.spacing.button_padding = egui::vec2(12.0, 7.0);
}

fn section_header(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text.to_uppercase()).small().strong().color(Color32::GRAY));
}

#[derive(Default)]
struct CardActions {
    connect: bool,
    favorite: bool,
    remove: bool,
}

/// Dibuja una tarjeta de equipo y devuelve qué botones se pulsaron.
/// `favorite` es `None` para equipos de la red (sin estrella/quitar) o `Some(fav)` para recientes.
fn device_card(ui: &mut egui::Ui, name: &str, addr: &str, online: bool, busy: bool, favorite: Option<bool>) -> CardActions {
    let mut act = CardActions::default();
    Frame::group(ui.style()).corner_radius(10).inner_margin(12).show(ui, |ui| {
        ui.set_width(168.0);
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("🖥").size(22.0));
                ui.add_space(2.0);
                ui.colored_label(if online { GREEN } else { Color32::from_gray(110) }, "●");
                if busy {
                    ui.label(RichText::new("ocupado").small().color(ORANGE));
                }
            });
            ui.add_space(4.0);
            ui.label(RichText::new(truncate(name, 22)).strong().size(15.0));
            ui.label(RichText::new(addr).small().color(Color32::GRAY));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                act.connect = ui.button(RichText::new("Conectar").strong()).clicked();
                if let Some(fav) = favorite {
                    if ui.button(if fav { "★" } else { "☆" }).on_hover_text("Favorito").clicked() {
                        act.favorite = true;
                    }
                    if ui.button("✕").on_hover_text("Quitar de recientes").clicked() {
                        act.remove = true;
                    }
                }
            });
        });
    });
    act
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
        PointerButton::Extra1 => Some(MouseButton::Back),
        PointerButton::Extra2 => Some(MouseButton::Forward),
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
