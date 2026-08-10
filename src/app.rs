// приложение (GUI)

pub mod dialogs;
pub mod events;
pub mod hotkey;
pub mod theme;

use crate::config::{Config, PeerInfo, Peers, peers_path, save_config};
use crate::logging::log_buffer;
use crate::network::client::{ping_status, response_message, response_ok, send_command};
use crate::network::server::spawn_server;
use crate::{log_info, log_warn};
use dialogs::AddDialog;
use eframe::egui::{self, Color32, CornerRadius, RichText};
use events::{StatusEvent, TOAST_LIFETIME, Toast};
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use hotkey::{HotkeyCapture, egui_key_to_code, egui_modifiers_to_hk};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};
use theme::{
    COLOR_ACCENT, COLOR_DANGER, COLOR_GRAY, COLOR_RED, COLOR_YELLOW, build_visuals, status_badge,
};

const DEFAULT_SECONDS: u64 = 15;

pub struct PeerApp {
    cfg: Arc<RwLock<Config>>,
    peers: Peers,
    statuses: BTreeMap<String, (String, Color32)>,
    seconds_input: String,
    ctx: egui::Context,
    tx: Sender<StatusEvent>,
    rx: Receiver<StatusEvent>,
    add_dialog: Option<AddDialog>,
    confirm_remove: Option<String>,
    errors: Vec<(String, String)>,
    icon_texture: egui::TextureHandle,
    hotkey_manager: Option<GlobalHotKeyManager>,
    active_hotkey: Option<HotKey>,
    hotkey_active_id: Arc<Mutex<Option<u32>>>,
    hotkey_capture: Option<HotkeyCapture>,
    toasts: Vec<Toast>,
    show_about: bool,
}

impl PeerApp {
    pub fn new(cc: &eframe::CreationContext<'_>, cfg: Arc<RwLock<Config>>, peers: Peers) -> Self {
        // Тёмная тема всегда, независимо от настроек ОС: и внутренняя тема
        // egui (иначе она следует за системной и на светлой теме съедала бы
        // наши цвета), и системная рамка окна (заголовок, красим напрямую
        // через DWM, а не через приватный API winit - см. apply_dark_titlebar).
        cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
        cc.egui_ctx.set_visuals(build_visuals());
        if let Some(window) = cc.winit_window() {
            theme::apply_dark_titlebar(window.as_ref());
        }

        let (tx, rx) = mpsc::channel();

        spawn_server(cfg.clone(), cc.egui_ctx.clone(), tx.clone());

        let (rgba, width, height) = theme::decode_icon();
        let color_image =
            egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &rgba);
        let icon_texture =
            cc.egui_ctx
                .load_texture("app_icon", color_image, egui::TextureOptions::LINEAR);

        let hotkey_manager = GlobalHotKeyManager::new().ok();
        let hotkey_active_id: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
        let mut active_hotkey = None;
        let mut startup_error = None;
        let saved_hotkey = cfg.read().unwrap().hotkey;
        if let (Some(manager), Some(saved)) = (&hotkey_manager, saved_hotkey) {
            match manager.register(saved) {
                Ok(()) => {
                    active_hotkey = Some(saved);
                    *hotkey_active_id.lock().unwrap() = Some(saved.id);
                }
                Err(exc) => {
                    startup_error = Some((
                        "Горячие клавиши".to_string(),
                        format!("Не удалось назначить сохранённую комбинацию «{saved}»: {exc}"),
                    ));
                }
            }
        }

        {
            let hotkey_active_id = hotkey_active_id.clone();
            let tx = tx.clone();
            let ctx = cc.egui_ctx.clone();
            thread::spawn(move || {
                let receiver = GlobalHotKeyEvent::receiver();
                while let Ok(event) = receiver.recv() {
                    if event.state() != HotKeyState::Pressed {
                        continue;
                    }
                    let matches = hotkey_active_id
                        .lock()
                        .is_ok_and(|guard| *guard == Some(event.id()));
                    if matches {
                        let _ = tx.send(StatusEvent::HotkeyTriggered);
                        ctx.request_repaint();
                    }
                }
            });
        }

        let mut app = Self {
            cfg,
            peers,
            statuses: BTreeMap::new(),
            seconds_input: DEFAULT_SECONDS.to_string(),
            ctx: cc.egui_ctx.clone(),
            tx,
            rx,
            add_dialog: None,
            confirm_remove: None,
            errors: Vec::new(),
            icon_texture,
            hotkey_manager,
            active_hotkey,
            hotkey_active_id,
            hotkey_capture: None,
            toasts: Vec::new(),
            show_about: false,
        };
        if let Some(error) = startup_error {
            app.errors.push(error);
        }
        app.refresh_all_statuses();
        app
    }

    // ---------- сохранение данных ----------

    fn token(&self) -> String {
        self.cfg.read().unwrap().token.clone()
    }

    fn save_peers(&self) {
        let text = serde_json::to_string_pretty(&self.peers).expect("serialize peers");
        std::fs::write(peers_path(), text).expect("write peers.json");
    }

    fn save_config(&self) {
        save_config(&self.cfg.read().unwrap());
    }

    // ---------- горячая клавиша ----------

    fn set_hotkey(&mut self, hotkey: HotKey) {
        let Some(manager) = &self.hotkey_manager else {
            self.errors.push((
                "Горячие клавиши".to_string(),
                "Менеджер горячих клавиш не инициализирован.".to_string(),
            ));
            return;
        };
        if let Some(old) = self.active_hotkey.take() {
            let _ = manager.unregister(old);
        }
        match manager.register(hotkey) {
            Ok(()) => {
                self.active_hotkey = Some(hotkey);
                if let Ok(mut guard) = self.hotkey_active_id.lock() {
                    *guard = Some(hotkey.id);
                }
                self.cfg.write().unwrap().hotkey = Some(hotkey);
                self.save_config();
            }
            Err(exc) => {
                self.errors.push((
                    "Горячие клавиши".to_string(),
                    format!("Не удалось назначить комбинацию «{hotkey}»: {exc}"),
                ));
            }
        }
    }

    fn clear_hotkey(&mut self) {
        if let Some(manager) = &self.hotkey_manager
            && let Some(old) = self.active_hotkey.take()
        {
            let _ = manager.unregister(old);
        }
        if let Ok(mut guard) = self.hotkey_active_id.lock() {
            *guard = None;
        }
        self.cfg.write().unwrap().hotkey = None;
        self.save_config();
    }

    // ---------- действия ----------

    fn seconds(&self) -> u64 {
        self.seconds_input
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|&s| s >= 1)
            .unwrap_or(DEFAULT_SECONDS)
    }

    fn refresh_all_statuses(&mut self) {
        for name in self.peers.keys().cloned().collect::<Vec<_>>() {
            self.spawn_ping(name);
        }
    }

    fn spawn_ping(&self, name: String) {
        let Some(peer) = self.peers.get(&name).cloned() else {
            return;
        };
        let token = self.token();
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        thread::spawn(move || {
            let (text, color) = ping_status(&peer, &token);
            let _ = tx.send(StatusEvent::Status { name, text, color });
            ctx.request_repaint();
        });
    }

    fn reboot_one(&mut self, name: String) {
        let seconds = self.seconds();
        self.statuses
            .insert(name.clone(), ("перезагрузка...".to_string(), COLOR_YELLOW));
        self.spawn_reboot(name, seconds);
    }

    fn spawn_reboot(&self, name: String, seconds: u64) {
        let Some(peer) = self.peers.get(&name).cloned() else {
            return;
        };
        let token = self.token();
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        log_info!(
            "sending reboot_internet to {name} ({}:{}) for {seconds}s",
            peer.host,
            peer.port
        );
        thread::spawn(move || {
            let result = send_command(
                &peer.host,
                peer.port,
                &token,
                json!({"action": "reboot_internet", "seconds": seconds}),
                Duration::from_secs_f32(6.0),
            );
            match result {
                Ok(resp) if response_ok(&resp) => {
                    log_info!("reboot_internet to {name} -> ok");
                    let _ = tx.send(StatusEvent::Status {
                        name: name.clone(),
                        text: format!("пауза {seconds}с"),
                        color: COLOR_YELLOW,
                    });
                    ctx.request_repaint();

                    thread::sleep(Duration::from_millis(seconds * 1000 + 1500));
                    let (text, color) = ping_status(&peer, &token);
                    let _ = tx.send(StatusEvent::Status { name, text, color });
                    ctx.request_repaint();
                }
                Ok(resp) => {
                    let message = response_message(&resp);
                    log_warn!("reboot_internet to {name} -> {message}");
                    let _ = tx.send(StatusEvent::Error {
                        title: name.clone(),
                        message,
                    });
                    let _ = tx.send(StatusEvent::Status {
                        name,
                        text: "ошибка".to_string(),
                        color: COLOR_RED,
                    });
                    ctx.request_repaint();
                }
                Err(exc) => {
                    log_warn!("reboot_internet to {name} -> could not connect: {exc}");
                    let _ = tx.send(StatusEvent::Error {
                        title: name.clone(),
                        message: format!("Не удалось подключиться: {exc}"),
                    });
                    let _ = tx.send(StatusEvent::Status {
                        name,
                        text: "недоступен".to_string(),
                        color: COLOR_RED,
                    });
                    ctx.request_repaint();
                }
            }
        });
    }

    fn reboot_all(&mut self) {
        let seconds = self.seconds();
        for name in self.peers.keys().cloned().collect::<Vec<_>>() {
            self.statuses
                .insert(name.clone(), ("перезагрузка...".to_string(), COLOR_YELLOW));
            self.spawn_reboot(name, seconds);
        }
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                StatusEvent::Status { name, text, color } => {
                    self.statuses.insert(name, (text, color));
                }
                StatusEvent::Error { title, message } => {
                    self.errors.push((title, message));
                }
                StatusEvent::HotkeyTriggered => {
                    self.reboot_all();
                }
                StatusEvent::Toast { message } => {
                    self.toasts.push(Toast {
                        message,
                        created_at: Instant::now(),
                    });
                }
            }
        }
    }

    // ---------- элементы UI ----------

    fn show_row(&mut self, ui: &mut egui::Ui, name: &str) {
        let peer = self.peers[name].clone();
        let (status_text, status_color) = self
            .statuses
            .get(name)
            .cloned()
            .unwrap_or_else(|| ("проверка...".to_string(), COLOR_GRAY));

        egui::Frame::new()
            .fill(ui.visuals().faint_bg_color)
            .corner_radius(CornerRadius::same(10))
            .inner_margin(egui::Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(name).strong().size(15.0));
                        ui.label(
                            RichText::new(format!("{}:{}", peer.host, peer.port))
                                .color(ui.visuals().weak_text_color())
                                .size(12.0),
                        );
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Удалить").clicked() {
                            self.confirm_remove = Some(name.to_string());
                        }
                        if ui
                            .add(
                                egui::Button::new("⚙ Редактировать")
                                    .fill(Color32::ORANGE.linear_multiply(0.25)),
                            )
                            .clicked()
                        {
                            self.add_dialog = Some(AddDialog::edit(name, &peer));
                        }
                        if ui
                            .add(
                                egui::Button::new("🔄 Перезапуск интернета")
                                    .fill(COLOR_ACCENT.linear_multiply(0.25)),
                            )
                            .clicked()
                        {
                            self.reboot_one(name.to_string());
                        }
                        ui.add_space(4.0);
                        status_badge(ui, &status_text, status_color);
                    });
                });
            });
        ui.add_space(6.0);
    }

    fn show_add_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.add_dialog else {
            return;
        };
        let mut close = false;
        let mut submit = false;

        egui::Modal::new(egui::Id::new("add_peer_modal")).show(ctx, |ui| {
            ui.set_min_width(320.0);

            if dialog.is_edit {
                ui.heading("Редактирование узла");
            } else {
                ui.heading("Новый узел сети");
            }
            ui.add_space(8.0);

            ui.label("Введите данные узла, который вы хотите добавить в список. Узел должен быть запущен и доступен по сети. \n\n\
            Если вы добавляете свой компьютер, используйте адрес 127.0.0.1 или localhost, порт 5990. \n\n\
            Если вы добавляете другой компьютер в сети, используйте его айпи из RadminVPN и порт 5990 (по умолчанию). \n");

            egui::Grid::new("add_peer_grid")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label(RichText::new("Название:"));
                    ui.text_edit_singleline(&mut dialog.name);
                    ui.end_row();

                    ui.label(RichText::new("ip (host):"));
                    ui.text_edit_singleline(&mut dialog.host);
                    ui.end_row();

                    ui.label(RichText::new("Порт:"));
                    ui.text_edit_singleline(&mut dialog.port);
                    ui.end_row();
                });

            if let Some(err) = &dialog.error {
                ui.add_space(6.0);
                ui.colored_label(COLOR_RED, err);
            }

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let button_text = if dialog.is_edit { "Сохранить" } else { "Добавить" };
                if ui.button(button_text).clicked() {
                    submit = true;
                }
                if ui.button("Отмена").clicked() {
                    close = true;
                }
            });
        });

        // ЗАКРЫВАЕМ диалог, если нажата кнопка "Отмена"
        if close {
            self.add_dialog = None;
            return;
        }

        // Если не нажата кнопка "Добавить/Сохранить" - выходим
        if !submit {
            return;
        }

        let dialog = self.add_dialog.take().unwrap();
        let is_edit = dialog.is_edit;
        let original_name = dialog.original_name.clone();
        let name = dialog.name.trim().to_string();
        let mut host = dialog.host.trim().to_string();
        let port: Option<u16> = dialog.port.trim().parse().ok();

        if host == "localhost" {
            host = "127.0.0.1".to_string();
        }

        // Проверяем ошибки
        if name.is_empty() {
            let mut new_dialog = dialog;
            new_dialog.error = Some("Введите название компьютера.".to_string());
            self.add_dialog = Some(new_dialog);
            return;
        } else if host.is_empty() {
            let mut new_dialog = dialog;
            new_dialog.error = Some("Введите RadminVPN-адрес.".to_string());
            self.add_dialog = Some(new_dialog);
            return;
        } else if port.is_none() {
            let mut new_dialog = dialog;
            new_dialog.error = Some("Введите корректный порт.".to_string());
            self.add_dialog = Some(new_dialog);
            return;
        }

        // Если это добавление нового узла - проверяем, что имя не занято
        if !is_edit && self.peers.contains_key(&name) {
            let mut new_dialog = dialog;
            new_dialog.error = Some("Узел с таким названием уже добавлен.".to_string());
            self.add_dialog = Some(new_dialog);
            return;
        }

        // Если это редактирование - удаляем старую запись
        if is_edit {
            self.peers.remove(&original_name);
            self.statuses.remove(&original_name);
        }

        // Добавляем новую запись
        self.peers.insert(
            name.clone(),
            PeerInfo {
                host,
                port: port.unwrap(),
            },
        );
        self.save_peers();

        // Обновляем статус
        self.spawn_ping(name);
    }

    fn show_confirm_remove(&mut self, ctx: &egui::Context) {
        let Some(name) = self.confirm_remove.clone() else {
            return;
        };
        let mut close = false;
        let mut remove = false;

        egui::Modal::new(egui::Id::new("confirm_remove_modal")).show(ctx, |ui| {
            ui.set_min_width(280.0);
            ui.label(format!("Удалить «{name}» из списка?"));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Да").clicked() {
                    remove = true;
                }
                if ui.button("Нет").clicked() {
                    close = true;
                }
            });
        });

        if remove {
            self.peers.remove(&name);
            self.statuses.remove(&name);
            self.save_peers();
            self.confirm_remove = None;
        } else if close {
            self.confirm_remove = None;
        }
    }

    fn show_about_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_about {
            return;
        }
        let mut close = false;

        egui::Modal::new(egui::Id::new("about_modal")).show(ctx, |ui| {
            ui.set_min_width(320.0);
            ui.horizontal(|ui| {
                ui.add(egui::Image::new((
                    self.icon_texture.id(),
                    egui::vec2(28.0, 28.0),
                )));
                ui.add_space(8.0);
                ui.heading("Networked Program Peer");
            });
            ui.label(RichText::new(env!("APP_VERSION")).weak().size(12.0));
            ui.add_space(10.0);

            ui.label(RichText::new("Автор").strong());
            ui.label("Discord: schrodinger71");
            ui.add_space(10.0);

            ui.label(RichText::new("Спонсоры").strong());
            ui.label("Anagirii — Discord: anagiri");
            ui.hyperlink_to("GitHub: Anagirii", "https://github.com/Anagirii");
            ui.add_space(10.0);

            ui.label(RichText::new("Лицензия").strong());
            ui.label("AGPL-3.0-or-later");
            ui.add_space(10.0);

            ui.hyperlink_to(
                "GitHub: Schrodinger71/peer-control-rs",
                "https://github.com/Schrodinger71/peer-control-rs",
            );
            ui.add_space(10.0);

            ui.label(
                RichText::new(
                    "Проект сделан в учебных целях. Автор не несёт ответственности \
                    за любые последствия использования этой программы, включая ущерб, \
                    причинённый её работой самому пользователю или третьим лицам.",
                )
                .color(ui.visuals().weak_text_color())
                .size(11.0),
            );

            ui.add_space(12.0);
            if ui.button("Закрыть").clicked() {
                close = true;
            }
        });

        if close {
            self.show_about = false;
        }
    }

    fn show_hotkey_capture(&mut self, ctx: &egui::Context) {
        let Some(capture) = &mut self.hotkey_capture else {
            return;
        };

        ctx.input(|i| {
            for event in &i.events {
                if let egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    repeat: false,
                    ..
                } = event
                    && let Some(code) = egui_key_to_code(*key)
                {
                    capture.captured = Some(HotKey::new(egui_modifiers_to_hk(*modifiers), code));
                }
            }
        });

        let mut close = false;
        let mut submit = false;

        egui::Modal::new(egui::Id::new("hotkey_capture_modal")).show(ctx, |ui| {
            ui.set_min_width(340.0);
            ui.heading("Горячая клавиша для «Перезагрузить у всех»");
            ui.add_space(8.0);
            match capture.captured {
                Some(hk) => {
                    ui.label(RichText::new(hk.to_string()).strong().size(16.0));
                }
                None => {
                    ui.label("Нажмите нужную комбинацию клавиш (например, Ctrl+Shift+5)...");
                }
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(capture.captured.is_some(), egui::Button::new("Сохранить"))
                    .clicked()
                {
                    submit = true;
                }
                if ui.button("Отмена").clicked() {
                    close = true;
                }
            });
        });

        let captured = capture.captured;

        if submit {
            if let Some(hotkey) = captured {
                self.set_hotkey(hotkey);
            }
            self.hotkey_capture = None;
        } else if close {
            self.hotkey_capture = None;
        }
    }

    fn show_toasts(&mut self, ctx: &egui::Context) {
        self.toasts
            .retain(|toast| toast.created_at.elapsed() < TOAST_LIFETIME);

        for (index, toast) in self.toasts.iter().enumerate() {
            egui::Area::new(egui::Id::new(("toast", index)))
                .anchor(
                    egui::Align2::RIGHT_TOP,
                    egui::vec2(-16.0, 16.0 + index as f32 * 56.0),
                )
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::Frame::new()
                        .fill(COLOR_ACCENT.linear_multiply(0.9))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(egui::Margin::symmetric(14, 10))
                        .show(ui, |ui| {
                            ui.set_max_width(320.0);
                            ui.label(RichText::new(&toast.message).color(Color32::WHITE));
                        });
                });
        }

        if !self.toasts.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    fn show_error_dialog(&mut self, ctx: &egui::Context) {
        let Some((title, message)) = self.errors.first().cloned() else {
            return;
        };
        let mut close = false;

        egui::Modal::new(egui::Id::new("error_modal")).show(ctx, |ui| {
            ui.set_min_width(300.0);
            ui.heading(&title);
            ui.add_space(8.0);
            ui.label(&message);
            ui.add_space(10.0);
            if ui.button("OK").clicked() {
                close = true;
            }
        });

        if close {
            self.errors.remove(0);
        }
    }

    fn show_log_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Журнал").strong());
            ui.label(
                RichText::new("(и входящие, и исходящие команды)")
                    .color(ui.visuals().weak_text_color())
                    .size(11.0),
            );
        });
        ui.add_space(4.0);
        egui::Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .max_height(220.0)
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        if let Ok(buf) = log_buffer().lock() {
                            for line in buf.iter() {
                                ui.label(RichText::new(line).monospace().size(11.0));
                            }
                        }
                    });
            });
    }
}

impl eframe::App for PeerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_events();
        let ctx = ui.ctx().clone();

        egui::Panel::top("top_bar")
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::symmetric(16, 14)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::Image::new((
                        self.icon_texture.id(),
                        egui::vec2(28.0, 28.0),
                    )));
                    ui.add_space(8.0);
                    ui.heading(RichText::new("Networked Program Peer").strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new("+ Добавить узел")
                                    .fill(COLOR_ACCENT.linear_multiply(0.35)),
                            )
                            .clicked()
                        {
                            self.add_dialog = Some(AddDialog::new());
                        }
                        if ui.button("↻ Обновить статус").clicked() {
                            self.refresh_all_statuses();
                        }
                        if ui
                            .add(
                                egui::Button::new("ℹ О программе")
                                    .fill(Color32::GOLD.linear_multiply(0.35)),
                            )
                            .clicked()
                        {
                            self.show_about = true;
                        }
                    });
                });
                ui.label(RichText::new(env!("APP_VERSION")).weak().size(12.0));
            });

        egui::Panel::bottom("log_panel")
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::symmetric(16, 10)),
            )
            .default_size(240.0)
            .show(ui, |ui| {
                self.show_log_panel(ui);
            });

        egui::Panel::bottom("controls_bar")
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::symmetric(16, 12)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Длительность отключения (сек):");
                    ui.add(egui::TextEdit::singleline(&mut self.seconds_input).desired_width(50.0));
                    ui.add_space(12.0);
                    ui.label("Горячая клавиша:");
                    match self.active_hotkey {
                        Some(hk) => {
                            ui.label(RichText::new(hk.to_string()).strong());
                            if ui.small_button("✕").clicked() {
                                self.clear_hotkey();
                            }
                        }
                        None => {
                            ui.label(
                                RichText::new("не назначена").color(ui.visuals().weak_text_color()),
                            );
                        }
                    }
                    if ui.small_button("Назначить").clicked() {
                        self.hotkey_capture = Some(HotkeyCapture::new());
                    }
                });
                ui.add_space(8.0);
                let reboot_all_btn = egui::Button::new(
                    RichText::new("🔄 Перезагрузить интернет у всех").color(Color32::WHITE),
                )
                .fill(COLOR_DANGER)
                .corner_radius(CornerRadius::same(8));
                if ui
                    .add_sized([ui.available_width(), 34.0], reboot_all_btn)
                    .clicked()
                    && !self.peers.is_empty()
                {
                    self.reboot_all();
                }
                ui.add_space(4.0);
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().extreme_bg_color)
                    .inner_margin(egui::Margin::symmetric(14, 12)),
            )
            .show(ui, |ui| {
                if self.peers.is_empty() {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| {
                        ui.label("Нет добавленных узлов. Нажмите «+ Добавить узел».");
                    });
                    return;
                }

                egui::ScrollArea::vertical().show(ui, |ui| {
                    for name in self.peers.keys().cloned().collect::<Vec<_>>() {
                        self.show_row(ui, &name);
                    }
                });
            });

        self.show_add_dialog(&ctx);
        self.show_confirm_remove(&ctx);
        self.show_hotkey_capture(&ctx);
        self.show_about_dialog(&ctx);
        self.show_error_dialog(&ctx);
        self.show_toasts(&ctx);
    }
}
