use std::collections::BTreeSet;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use edgestash_ipc::config::{
    AreaAwareScrollMode, ChurnZoneAction, DragDropModifierKey, EdgeStashConfig, GestureMode,
};
use edgestash_ipc::protocol::{DEFAULT_PIPE_NAME, EdgeSlotSnapshot, IpcRequest, IpcResponse};
use edgestash_ipc::types::{
    AnimationStyle, ChurnZoneId, DockMode, EdgePillStyle, ScreenEdge, VirtualDesktopMode,
};
use egui::{Color32, CornerRadius, Frame, Margin, Stroke, Vec2};

use crate::hotkey::{HotkeyCaptureOutcome, HotkeyField, process_captured_key};
use crate::ipc_bridge::{DaemonClient, PipeDaemonClient, launch_daemon_process};
use crate::model::{BannerKind, DaemonConnection, EdgeStashUiModel, StatusBanner, UiTab};
use crate::theme::{Palette, ThemeMode, accent_button, apply_theme, badge_pill, card, toggle_row};

enum WorkerJob {
    Poll,
    Action(IpcRequest),
    PreviewChurnVisual(IpcRequest),
    ReloadConfig,
}

enum WorkerEvent {
    Polled {
        ping: std::io::Result<IpcResponse>,
        status: Option<std::io::Result<IpcResponse>>,
    },
    ActionCompleted {
        was_shutdown: bool,
        action_res: std::io::Result<IpcResponse>,
        refresh_res: Option<std::io::Result<IpcResponse>>,
    },
    ReloadCompleted(std::io::Result<IpcResponse>),
}

/// Interactive `eframe` desktop GUI for EdgeStash settings & live slot control.
pub struct EdgeStashApp {
    model: EdgeStashUiModel,
    theme_mode: ThemeMode,
    job_tx: mpsc::Sender<WorkerJob>,
    event_rx: mpsc::Receiver<WorkerEvent>,
    poll_in_flight: bool,
    last_poll_at: Instant,
    confirm_shutdown: bool,
    running_apps: Vec<(String, String)>,
    running_apps_refreshed_at: Instant,
    pub show_churn_visualizer: bool,
    pub preview_zone: Option<ChurnZoneId>,
    last_churn_preview_params: Option<(bool, Option<ChurnZoneId>, i32, i32, i32)>,
    save_debounce_deadline: Option<Instant>,
    pub show_save_preset_dialog: bool,
    pub preset_name_input: String,
    pub preset_save_error: Option<String>,
}

impl EdgeStashApp {
    /// Initializes the GUI ViewModel, loads `config.toml`, and spawns the IPC worker thread.
    pub fn new(cc: &eframe::CreationContext<'_>, model: EdgeStashUiModel) -> Self {
        let (job_tx, job_rx) = mpsc::channel::<WorkerJob>();
        let (event_tx, event_rx) = mpsc::channel::<WorkerEvent>();
        let ctx_clone = cc.egui_ctx.clone();

        thread::spawn(move || {
            let client = PipeDaemonClient::new(DEFAULT_PIPE_NAME);
            while let Ok(job) = job_rx.recv() {
                match job {
                    WorkerJob::Poll => {
                        let ping = client.send_request(&IpcRequest::Ping);
                        let status = match &ping {
                            Ok(IpcResponse::Pong { .. }) => {
                                Some(client.send_request(&IpcRequest::Status))
                            }
                            _ => None,
                        };
                        let _ = event_tx.send(WorkerEvent::Polled { ping, status });
                    }
                    WorkerJob::Action(req) => {
                        let was_shutdown = matches!(req, IpcRequest::Shutdown);
                        let action_res = client.send_request(&req);
                        let refresh_res = match (&action_res, was_shutdown) {
                            (Ok(IpcResponse::Ok { .. }), false) => {
                                Some(client.send_request(&IpcRequest::Status))
                            }
                            _ => None,
                        };
                        let _ = event_tx.send(WorkerEvent::ActionCompleted {
                            was_shutdown,
                            action_res,
                            refresh_res,
                        });
                    }
                    WorkerJob::PreviewChurnVisual(mut req) => {
                        let mut next_job = None;
                        while let Ok(pending) = job_rx.try_recv() {
                            match pending {
                                WorkerJob::PreviewChurnVisual(newer) => {
                                    req = newer;
                                }
                                other => {
                                    next_job = Some(other);
                                    break;
                                }
                            }
                        }
                        let _ = client.send_request(&req);
                        if let Some(other) = next_job {
                            match other {
                                WorkerJob::Poll => {
                                    let ping = client.send_request(&IpcRequest::Ping);
                                    let status = match &ping {
                                        Ok(IpcResponse::Pong { .. }) => {
                                            Some(client.send_request(&IpcRequest::Status))
                                        }
                                        _ => None,
                                    };
                                    let _ = event_tx.send(WorkerEvent::Polled { ping, status });
                                }
                                WorkerJob::Action(act_req) => {
                                    let was_shutdown = matches!(act_req, IpcRequest::Shutdown);
                                    let action_res = client.send_request(&act_req);
                                    let refresh_res = match (&action_res, was_shutdown) {
                                        (Ok(IpcResponse::Ok { .. }), false) => {
                                            Some(client.send_request(&IpcRequest::Status))
                                        }
                                        _ => None,
                                    };
                                    let _ = event_tx.send(WorkerEvent::ActionCompleted {
                                        was_shutdown,
                                        action_res,
                                        refresh_res,
                                    });
                                }
                                WorkerJob::ReloadConfig => {
                                    let res = client.send_request(&IpcRequest::ReloadConfig);
                                    let _ = event_tx.send(WorkerEvent::ReloadCompleted(res));
                                }
                                WorkerJob::PreviewChurnVisual(_) => {}
                            }
                        }
                    }
                    WorkerJob::ReloadConfig => {
                        let res = client.send_request(&IpcRequest::ReloadConfig);
                        let _ = event_tx.send(WorkerEvent::ReloadCompleted(res));
                    }
                }
                ctx_clone.request_repaint();
            }
        });

        let _ = job_tx.send(WorkerJob::Poll);

        Self {
            model,
            theme_mode: ThemeMode::Dark,
            job_tx,
            event_rx,
            poll_in_flight: true,
            last_poll_at: Instant::now(),
            confirm_shutdown: false,
            running_apps: enumerate_running_app_candidates(),
            running_apps_refreshed_at: Instant::now(),
            show_churn_visualizer: false,
            preview_zone: Some(ChurnZoneId::TopRightDown),
            last_churn_preview_params: None,
            save_debounce_deadline: None,
            show_save_preset_dialog: false,
            preset_name_input: String::new(),
            preset_save_error: None,
        }
    }

    fn sync_churn_visualizer_if_needed(&mut self) {
        let current = (
            self.show_churn_visualizer,
            self.preview_zone,
            self.model.draft_config.corner_zone_px,
            self.model.draft_config.slide_distance_px,
            self.model.draft_config.churn_diagonal_angle_deg,
        );
        if self.last_churn_preview_params != Some(current) {
            self.last_churn_preview_params = Some(current);
            let _ = self.job_tx.send(WorkerJob::PreviewChurnVisual(
                IpcRequest::PreviewChurnVisual {
                    enabled: current.0,
                    zone: current.1,
                    corner_zone_px: current.2,
                    slide_distance_px: current.3,
                    diagonal_angle_deg: current.4,
                },
            ));
        }
    }

    fn queue_poll(&mut self) {
        if !self.poll_in_flight && self.job_tx.send(WorkerJob::Poll).is_ok() {
            self.poll_in_flight = true;
            self.last_poll_at = Instant::now();
        }
    }

    fn queue_action(&mut self, req: IpcRequest) {
        let _ = self.job_tx.send(WorkerJob::Action(req));
    }

    fn save_and_apply(&mut self) {
        self.save_debounce_deadline = None;
        if self.model.save_draft_to_disk().is_ok() {
            let _ = self.job_tx.send(WorkerJob::ReloadConfig);
        }
    }

    fn drain_worker_events(&mut self) {
        while let Ok(ev) = self.event_rx.try_recv() {
            match ev {
                WorkerEvent::Polled { ping, status } => {
                    self.poll_in_flight = false;
                    self.model.apply_poll_outcome(ping, status);
                }
                WorkerEvent::ActionCompleted {
                    was_shutdown,
                    action_res,
                    refresh_res,
                } => {
                    self.model
                        .apply_action_outcome(was_shutdown, action_res, refresh_res);
                }
                WorkerEvent::ReloadCompleted(res) => {
                    self.model.apply_reload_outcome(res);
                }
            }
        }
    }

    fn handle_hotkey_capture_input(&mut self, ctx: &egui::Context) {
        let Some(target) = self.model.capturing_hotkey else {
            return;
        };
        let captured_events: Vec<(egui::Modifiers, egui::Key)> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => Some((*modifiers, *key)),
                    _ => None,
                })
                .collect()
        });

        for (mods, key) in captured_events {
            match process_captured_key(mods, key) {
                HotkeyCaptureOutcome::Cancelled => {
                    self.model.capturing_hotkey = None;
                    break;
                }
                HotkeyCaptureOutcome::Captured(binding) => {
                    target.set(&mut self.model.draft_config.hotkeys, binding);
                    self.model.capturing_hotkey = None;
                    self.model.validate_draft();
                    break;
                }
                HotkeyCaptureOutcome::Pending => {}
            }
        }
    }

    fn render_top_header(&mut self, ctx: &egui::Context, p: &Palette) {
        egui::TopBottomPanel::top("edgestash_header")
            .frame(
                Frame::NONE
                    .fill(p.bg_sidebar)
                    .stroke(Stroke::new(1.0_f32, p.border))
                    .inner_margin(Margin::symmetric(16, 12)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("EdgeStash")
                                    .size(20.0)
                                    .strong()
                                    .color(p.text_primary),
                            );
                            ui.label(
                                egui::RichText::new("Universal Edge-Drawer Control Center")
                                    .size(12.5)
                                    .color(p.text_secondary),
                            );
                        });
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button(format!("Theme: {}", self.theme_mode.label()))
                            .clicked()
                        {
                            self.theme_mode = self.theme_mode.toggle();
                        }

                        if ui.button("Refresh").clicked() {
                            self.queue_poll();
                        }

                        if ui.button("💾 Save Preset").clicked() {
                            self.show_save_preset_dialog = true;
                            self.preset_name_input.clear();
                            self.preset_save_error = None;
                        }

                        let presets = EdgeStashConfig::list_presets();
                        if !presets.is_empty() {
                            egui::ComboBox::from_id_salt("top_presets_menu")
                                .selected_text("Presets ▾")
                                .show_ui(ui, |ui| {
                                    ui.label(
                                        egui::RichText::new("Saved Presets")
                                            .strong()
                                            .size(11.0)
                                            .color(p.text_secondary),
                                    );
                                    for (name, path) in &presets {
                                        if ui.button(format!("Load '{}'", name)).clicked() {
                                            match self.model.load_preset_from_disk(path) {
                                                Ok(()) => {
                                                    self.model.banner = Some(StatusBanner {
                                                        kind: BannerKind::Success,
                                                        message: format!(
                                                            "Loaded preset '{}' from disk",
                                                            name
                                                        ),
                                                    });
                                                    self.save_and_apply();
                                                }
                                                Err(err) => {
                                                    self.model.banner = Some(StatusBanner {
                                                        kind: BannerKind::Error,
                                                        message: err,
                                                    });
                                                }
                                            }
                                        }
                                    }
                                    ui.separator();
                                    if ui.button("📂 Open Presets Folder").clicked()
                                        && let Some(dir) = EdgeStashConfig::default_presets_dir()
                                    {
                                        let _ = std::fs::create_dir_all(&dir);
                                        let _ = std::process::Command::new("explorer")
                                            .arg(&dir)
                                            .spawn();
                                    }
                                });
                        }

                        match &self.model.connection {
                            DaemonConnection::Connected { version, pid } => {
                                badge_pill(
                                    ui,
                                    &format!("Daemon Online  v{version}  PID {pid}"),
                                    p.success_soft,
                                    p.success,
                                );
                            }
                            DaemonConnection::Disconnected { .. } => {
                                if accent_button(ui, p, "Start Daemon").clicked() {
                                    match launch_daemon_process() {
                                        Ok(()) => {
                                            self.model.banner = Some(StatusBanner {
                                                kind: BannerKind::Info,
                                                message: "Launched edgestash-daemon.exe — checking connection..."
                                                    .to_string(),
                                            });
                                            self.last_poll_at =
                                                Instant::now() - Duration::from_millis(1200);
                                        }
                                        Err(err) => {
                                            self.model.banner = Some(StatusBanner {
                                                kind: BannerKind::Error,
                                                message: err,
                                            });
                                        }
                                    }
                                }
                                badge_pill(ui, "Daemon Offline", p.warning_soft, p.warning);
                            }
                            DaemonConnection::Unknown => {
                                badge_pill(ui, "Connecting...", p.accent_soft, p.accent);
                            }
                        }
                    });
                });
            });
    }

    fn render_save_preset_dialog(&mut self, ctx: &egui::Context, p: &Palette) {
        if !self.show_save_preset_dialog {
            return;
        }

        let mut close_dialog = false;
        egui::Window::new("💾 Save Settings Preset")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .fixed_size(egui::vec2(420.0, 180.0))
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new("Enter a name for this preset:")
                        .size(13.0)
                        .strong()
                        .color(p.text_primary),
                );
                ui.add_space(4.0);

                let text_edit = ui.add(
                    egui::TextEdit::singleline(&mut self.preset_name_input)
                        .hint_text("e.g. Work, Gaming, Ultra-Minimal...")
                        .desired_width(ui.available_width()),
                );
                text_edit.request_focus();

                if let Some(err) = &self.preset_save_error {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(err).size(11.5).color(p.error));
                }

                let presets_dir = EdgeStashConfig::default_presets_dir();
                if let Some(dir) = &presets_dir {
                    ui.add_space(6.0);
                    let target_filename = if self.preset_name_input.trim().is_empty() {
                        "<name>.toml".to_string()
                    } else {
                        format!("{}.toml", self.preset_name_input.trim())
                    };
                    ui.label(
                        egui::RichText::new(format!(
                            "File will be generated at:\n{}\\{}",
                            dir.display(),
                            target_filename
                        ))
                        .size(10.5)
                        .color(p.text_secondary),
                    );
                }

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close_dialog = true;
                    }

                    if let Some(dir) = &presets_dir
                        && ui.button("📂 Open Folder").clicked()
                    {
                        let _ = std::fs::create_dir_all(dir);
                        let _ = std::process::Command::new("explorer").arg(dir).spawn();
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let save_clicked = accent_button(ui, p, "Save Preset").clicked()
                            || (text_edit.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter)));

                        if save_clicked {
                            let name = self.preset_name_input.trim().to_string();
                            match self.model.save_as_named_preset(&name) {
                                Ok(saved_path) => {
                                    self.model.banner = Some(StatusBanner {
                                        kind: BannerKind::Success,
                                        message: format!(
                                            "Saved preset '{}' to {}",
                                            name,
                                            saved_path.display()
                                        ),
                                    });
                                    close_dialog = true;
                                }
                                Err(err) => {
                                    self.preset_save_error = Some(err);
                                }
                            }
                        }
                    });
                });
            });

        if close_dialog {
            self.show_save_preset_dialog = false;
            self.preset_save_error = None;
        }
    }

    fn render_bottom_bar(&mut self, ctx: &egui::Context, p: &Palette) {
        egui::TopBottomPanel::bottom("edgestash_footer")
            .frame(
                Frame::NONE
                    .fill(p.bg_sidebar)
                    .stroke(Stroke::new(1.0_f32, p.border))
                    .inner_margin(Margin::symmetric(16, 10)),
            )
            .show(ctx, |ui| {
                if let Some(banner) = self.model.banner.clone() {
                    let (bg, fg) = match banner.kind {
                        BannerKind::Info => (p.accent_soft, p.accent),
                        BannerKind::Success => (p.success_soft, p.success),
                        BannerKind::Warning => (p.warning_soft, p.warning),
                        BannerKind::Error => (p.error_soft, p.error),
                    };
                    Frame::NONE
                        .fill(bg)
                        .corner_radius(CornerRadius::same(7))
                        .inner_margin(Margin::symmetric(10, 6))
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(&banner.message).size(12.5).color(fg));
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.small_button("Dismiss").clicked() {
                                            self.model.banner = None;
                                        }
                                    },
                                );
                            });
                        });
                    ui.add_space(6.0);
                }

                ui.horizontal(|ui| {
                    let path_text = self.model.config_path.display().to_string();
                    ui.label(
                        egui::RichText::new(format!("Config: {path_text}"))
                            .size(11.5)
                            .color(p.text_secondary),
                    )
                    .on_hover_text(path_text);

                    if !self.model.hotkey_errors.is_empty() {
                        badge_pill(ui, "Invalid hotkey", p.error_soft, p.error);
                    } else if !self.model.is_dirty() {
                        badge_pill(ui, "Auto-saved", p.success_soft, p.success);
                    } else {
                        badge_pill(ui, "Unsaved changes", p.accent_soft, p.accent);
                        if ui.button("⚡ Apply Now").clicked() {
                            self.save_and_apply();
                        }
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Reset Defaults").clicked() {
                            self.model.reset_to_defaults();
                            self.save_and_apply();
                        }
                    });
                });
            });
    }

    fn render_sidebar(&mut self, ctx: &egui::Context, p: &Palette) {
        egui::SidePanel::left("edgestash_nav")
            .resizable(false)
            .exact_width(198.0)
            .frame(
                Frame::NONE
                    .fill(p.bg_sidebar)
                    .stroke(Stroke::new(1.0_f32, p.border))
                    .inner_margin(Margin::same(12)),
            )
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new("NAVIGATION")
                        .size(11.0)
                        .strong()
                        .color(p.text_secondary),
                );
                ui.add_space(6.0);

                for tab in UiTab::ALL {
                    let selected = self.model.active_tab == tab;
                    let fill = if selected {
                        p.accent_soft
                    } else {
                        Color32::TRANSPARENT
                    };
                    let stroke = if selected {
                        Stroke::new(1.0_f32, p.accent)
                    } else {
                        Stroke::NONE
                    };
                    let text_color = if selected { p.accent } else { p.text_primary };

                    let btn = egui::Button::new(
                        egui::RichText::new(tab.title())
                            .size(13.5)
                            .strong()
                            .color(text_color),
                    )
                    .min_size(Vec2::new(ui.available_width(), 36.0))
                    .fill(fill)
                    .stroke(stroke)
                    .corner_radius(CornerRadius::same(8));

                    if ui.add(btn).clicked() {
                        self.model.active_tab = tab;
                    }
                }

                ui.add_space(16.0);
                ui.separator();
                ui.add_space(8.0);

                ui.label(
                    egui::RichText::new("ACTIVE SUMMARY")
                        .size(11.0)
                        .strong()
                        .color(p.text_secondary),
                );
                ui.add_space(4.0);
                let cfg = &self.model.draft_config;
                summary_kv(ui, p, "Trigger", gesture_label(cfg.gesture_mode));
                summary_kv(ui, p, "Animation", anim_label(cfg.transition_style));
                summary_kv(ui, p, "Default Dock", dock_label(cfg.dock_mode));
                summary_kv(ui, p, "Edge Pill", pill_label(cfg.edge_pill_style));
            });
    }

    fn render_content(&mut self, ctx: &egui::Context, p: &Palette) {
        egui::CentralPanel::default()
            .frame(Frame::NONE.fill(p.bg_app).inner_margin(Margin::same(18)))
            .show(ctx, |ui| {
                let tab = self.model.active_tab;
                ui.label(
                    egui::RichText::new(tab.title())
                        .size(21.0)
                        .strong()
                        .color(p.text_primary),
                );
                ui.label(
                    egui::RichText::new(tab.subtitle())
                        .size(12.5)
                        .color(p.text_secondary),
                );
                ui.add_space(12.0);

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| match tab {
                        UiTab::Dashboard => self.render_dashboard_tab(ui, p),
                        UiTab::Triggers => self.render_triggers_tab(ui, p),
                        UiTab::Behavior => self.render_behavior_tab(ui, p),
                        UiTab::Visuals => self.render_visuals_tab(ui, p),
                        UiTab::Hotkeys => self.render_hotkeys_tab(ui, p),
                    });
            });
    }

    fn render_dashboard_tab(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let slots: Vec<EdgeSlotSnapshot> = self
            .model
            .status_snapshot
            .as_ref()
            .map(|s| s.slots.clone())
            .unwrap_or_default();
        let total_windows: usize = slots.iter().map(|s| s.windows.len()).sum();
        let open_count = slots.iter().filter(|s| s.is_open).count();
        let is_connected = matches!(self.model.connection, DaemonConnection::Connected { .. });

        ui.horizontal_wrapped(|ui| {
            metric_tile(
                ui,
                p,
                "Stashed Windows",
                &total_windows.to_string(),
                p.accent,
            );
            metric_tile(
                ui,
                p,
                "Edge Slots",
                &slots.len().to_string(),
                p.text_primary,
            );
            metric_tile(
                ui,
                p,
                "Open Drawers",
                &open_count.to_string(),
                if open_count > 0 {
                    p.success
                } else {
                    p.text_secondary
                },
            );
            metric_tile(
                ui,
                p,
                "Trigger Mode",
                gesture_label(self.model.saved_config.gesture_mode),
                p.text_primary,
            );
        });

        ui.add_space(10.0);

        card(
            ui,
            p,
            "Daemon & Quick Edge Actions",
            Some("Control drawers across all monitors or unstash everything cleanly"),
            |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(is_connected, egui::Button::new("Toggle Active Drawer"))
                        .clicked()
                    {
                        self.queue_action(IpcRequest::ToggleActive);
                    }
                    if ui
                        .add_enabled(
                            is_connected && total_windows > 0,
                            egui::Button::new("Unstash All Windows"),
                        )
                        .clicked()
                    {
                        self.queue_action(IpcRequest::UnstashAll);
                    }
                    if ui
                        .add_enabled(is_connected, egui::Button::new("Reload Daemon Config"))
                        .clicked()
                    {
                        self.queue_action(IpcRequest::ReloadConfig);
                    }

                    if is_connected {
                        if self.confirm_shutdown {
                            ui.label(
                                egui::RichText::new("Stop background daemon?")
                                    .color(p.warning)
                                    .strong(),
                            );
                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("Confirm Stop").color(Color32::WHITE),
                                    )
                                    .fill(p.error),
                                )
                                .clicked()
                            {
                                self.confirm_shutdown = false;
                                self.queue_action(IpcRequest::Shutdown);
                            }
                            if ui.button("Cancel").clicked() {
                                self.confirm_shutdown = false;
                            }
                        } else if ui.button("Stop Daemon").clicked() {
                            self.confirm_shutdown = true;
                        }
                    }
                });

                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new("Quick Stash Active Window To:")
                            .size(12.0)
                            .color(p.text_secondary),
                    );
                    for edge in ScreenEdge::ALL {
                        let lbl = edge_short(edge);
                        if ui
                            .add_enabled(is_connected, egui::Button::new(lbl))
                            .clicked()
                        {
                            self.queue_action(IpcRequest::StashForeground {
                                edge,
                                group_with_active: false,
                            });
                        }
                    }
                });
            },
        );

        ui.add_space(10.0);

        if !is_connected {
            card(
                ui,
                p,
                "Daemon Is Not Running",
                Some(
                    "Start `edgestash-daemon.exe` from the header button (Windows will prompt UAC) to inspect or manage live stashed windows.",
                ),
                |ui| {
                    ui.label(
                        egui::RichText::new(
                            "You can still edit and save all configuration settings offline — they apply automatically on next launch.",
                        )
                        .color(p.text_secondary),
                    );
                },
            );
            return;
        }

        if slots.is_empty() {
            let hk = &self.model.saved_config.hotkeys;
            card(
                ui,
                p,
                "No Windows Currently Stashed",
                Some(
                    "Stash any window with your keyboard shortcut or by dragging it to a screen edge.",
                ),
                |ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "Hotkeys: Left ({})  |  Right ({})  |  Top ({})  |  Bottom ({})",
                            hk.stash_left, hk.stash_right, hk.stash_top, hk.stash_bottom
                        ))
                        .size(12.5)
                        .color(p.text_secondary),
                    );
                },
            );
            return;
        }

        // Count how many slots share each (monitor, edge) deck so Cycle buttons appear when stacked.
        for slot in slots {
            self.render_slot_card(ui, p, &slot);
            ui.add_space(8.0);
        }
    }

    fn render_slot_card(&mut self, ui: &mut egui::Ui, p: &Palette, slot: &EdgeSlotSnapshot) {
        let edge_name = edge_short(slot.edge);
        let mon_short = short_monitor_name(&slot.monitor_id);

        card(
            ui,
            p,
            &format!(
                "{edge_name} Edge  •  Slot #{}  ({mon_short})",
                slot.slot_index + 1
            ),
            None,
            |ui| {
                ui.horizontal_wrapped(|ui| {
                    if slot.is_open {
                        badge_pill(ui, "OPEN", p.success_soft, p.success);
                    } else {
                        badge_pill(ui, "STASHED", p.bg_card_alt, p.text_secondary);
                    }
                    if slot.is_active_in_deck {
                        badge_pill(ui, "ACTIVE", p.accent_soft, p.accent);
                    }
                    badge_pill(
                        ui,
                        dock_label(slot.dock_mode),
                        p.bg_card_alt,
                        p.text_primary,
                    );

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Unstash Edge").clicked() {
                            self.queue_action(IpcRequest::UnstashActive {
                                monitor_id: Some(slot.monitor_id.clone()),
                                edge: slot.edge,
                            });
                        }

                        let next_dock = match slot.dock_mode {
                            DockMode::FloatingOverlay => DockMode::FloatingPinned,
                            DockMode::FloatingPinned => DockMode::NativeWorkAreaDock,
                            DockMode::NativeWorkAreaDock => DockMode::FloatingOverlay,
                        };
                        let dock_btn_label = match slot.dock_mode {
                            DockMode::FloatingOverlay => "Pin (Floating)",
                            DockMode::FloatingPinned => "Dock (Native)",
                            DockMode::NativeWorkAreaDock => "Unpin",
                        };
                        if ui.button(dock_btn_label).clicked() {
                            self.queue_action(IpcRequest::SetDockMode {
                                monitor_id: Some(slot.monitor_id.clone()),
                                edge: slot.edge,
                                mode: next_dock,
                            });
                        }

                        if ui.button("Next").clicked() {
                            self.queue_action(IpcRequest::CycleEdge {
                                monitor_id: Some(slot.monitor_id.clone()),
                                edge: slot.edge,
                                delta: 1,
                            });
                        }
                        if ui.button("Prev").clicked() {
                            self.queue_action(IpcRequest::CycleEdge {
                                monitor_id: Some(slot.monitor_id.clone()),
                                edge: slot.edge,
                                delta: -1,
                            });
                        }

                        let toggle_lbl = if slot.is_open {
                            "Close Drawer"
                        } else {
                            "Open Drawer"
                        };
                        if accent_button(ui, p, toggle_lbl).clicked() {
                            self.queue_action(IpcRequest::ToggleEdge {
                                monitor_id: Some(slot.monitor_id.clone()),
                                edge: slot.edge,
                            });
                        }
                    });
                });

                ui.add_space(6.0);
                let multi_win = slot.windows.len() > 1;
                for win in &slot.windows {
                    Frame::NONE
                        .fill(p.bg_card_alt)
                        .corner_radius(CornerRadius::same(7))
                        .inner_margin(Margin::symmetric(10, 7))
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    let title = if win.title.is_empty() {
                                        "<Untitled Window>"
                                    } else {
                                        win.title.as_str()
                                    };
                                    ui.label(
                                        egui::RichText::new(title)
                                            .strong()
                                            .size(13.0)
                                            .color(p.text_primary),
                                    );
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "HWND 0x{:X}  •  {}x{} at ({}, {})",
                                            win.hwnd,
                                            win.target_rect.width(),
                                            win.target_rect.height(),
                                            win.target_rect.left,
                                            win.target_rect.top
                                        ))
                                        .size(11.5)
                                        .color(p.text_secondary),
                                    );
                                });

                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.button("Unstash").clicked() {
                                            self.queue_action(IpcRequest::UnstashWindow {
                                                hwnd: win.hwnd,
                                            });
                                        }
                                        if multi_win && ui.button("Ungroup").clicked() {
                                            self.queue_action(IpcRequest::UngroupWindow {
                                                hwnd: win.hwnd,
                                            });
                                        }
                                        let next_vd = match slot.virtual_desktop_mode {
                                            VirtualDesktopMode::AllDesktops => {
                                                VirtualDesktopMode::CurrentDesktopOnly
                                            }
                                            VirtualDesktopMode::CurrentDesktopOnly => {
                                                VirtualDesktopMode::AllDesktops
                                            }
                                        };
                                        let vd_btn = match slot.virtual_desktop_mode {
                                            VirtualDesktopMode::AllDesktops => "VD: All Desktops",
                                            VirtualDesktopMode::CurrentDesktopOnly => {
                                                "VD: Pinned Desktop"
                                            }
                                        };
                                        if ui.button(vd_btn).clicked() {
                                            self.queue_action(IpcRequest::SetVirtualDesktopMode {
                                                hwnd: win.hwnd,
                                                mode: next_vd,
                                            });
                                        }
                                    },
                                );
                            });
                        });
                    ui.add_space(4.0);
                }
            },
        );
    }

    fn render_triggers_tab(&mut self, ui: &mut egui::Ui, p: &Palette) {
        if self.running_apps_refreshed_at.elapsed() >= Duration::from_secs(3) {
            self.running_apps = enumerate_running_app_candidates();
            self.running_apps_refreshed_at = Instant::now();
        }
        let app_candidates =
            build_churn_app_candidates(self.model.status_snapshot.as_ref(), &self.running_apps);
        let mut manual_refresh = false;

        let cfg = &mut self.model.draft_config;
        card(
            ui,
            p,
            "Summoning Gesture Mode",
            Some("Choose how stashed drawers reveal from screen edges"),
            |ui| {
                let modes = [
                    (
                        GestureMode::EdgeBump,
                        "Edge Bump / Bezel Push (Recommended)",
                        "Push cursor firmly against any stashed screen edge (crisp 80ms threshold) to reveal. Zero corner aiming required.",
                    ),
                    (
                        GestureMode::CornerSlide,
                        "Corner Slide (Charms)",
                        "Enter an 8x8 corner zone and slide down/along the bezel. Zero false triggers near scrollbars or close buttons.",
                    ),
                    (
                        GestureMode::EdgeHover,
                        "Edge Hover",
                        "Rest the cursor anywhere on a stashed 1px screen edge for the dwell delay.",
                    ),
                    (
                        GestureMode::PillClick,
                        "Edge Pill / Click",
                        "Hover or click the visible edge indicator pill (wheel scroll cycles stacked drawers).",
                    ),
                    (
                        GestureMode::HotkeyOnly,
                        "Hotkey Only",
                        "Never trigger via pointer proximity — summon strictly via keyboard shortcuts.",
                    ),
                ];

                for (mode, title, desc) in modes {
                    let selected = cfg.gesture_mode == mode;
                    let fill = if selected {
                        p.accent_soft
                    } else {
                        p.bg_card_alt
                    };
                    let stroke = if selected {
                        Stroke::new(1.5_f32, p.accent)
                    } else {
                        Stroke::new(1.0_f32, p.border)
                    };
                    let resp = Frame::NONE
                        .fill(fill)
                        .stroke(stroke)
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(12, 9))
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.radio_value(&mut cfg.gesture_mode, mode, "");
                                ui.vertical(|ui| {
                                    ui.label(
                                        egui::RichText::new(title)
                                            .strong()
                                            .size(13.5)
                                            .color(p.text_primary),
                                    );
                                    ui.label(
                                        egui::RichText::new(desc)
                                            .size(11.5)
                                            .color(p.text_secondary),
                                    );
                                });
                            });
                        })
                        .response
                        .interact(egui::Sense::click());
                    if resp.clicked() {
                        cfg.gesture_mode = mode;
                    }
                    ui.add_space(4.0);
                }
            },
        );

        ui.add_space(10.0);

        card(
            ui,
            p,
            "Precision & Timing Tuning",
            Some("Ranges match EdgeStash safety bounds in config.toml"),
            |ui| {
                slider_row(
                    ui,
                    p,
                    "Edge Bump Threshold (ms)",
                    "Dwell threshold pushing against bezel before revealing stashed drawer",
                    &mut cfg.edge_bump_dwell_ms,
                    0..=300,
                    " ms",
                );
                slider_row(
                    ui,
                    p,
                    "Corner Zone Size (px)",
                    "Corner box thickness for CornerSlide entry",
                    &mut cfg.corner_zone_px,
                    4..=32,
                    " px",
                );
                slider_row(
                    ui,
                    p,
                    "Slide Distance (px)",
                    "Required movement along the edge after hitting a corner",
                    &mut cfg.slide_distance_px,
                    16..=200,
                    " px",
                );
                slider_row(
                    ui,
                    p,
                    "Corner Slide Window (ms)",
                    "Maximum time allowed between corner entry and slide completion",
                    &mut cfg.corner_timeout_ms,
                    150..=3000,
                    " ms",
                );
                slider_row(
                    ui,
                    p,
                    "Allowed Diagonal Angle (°)",
                    "Permitted inward corridor flare angle when sliding diagonally along corner bezels (outward bezel pushing is always allowed)",
                    &mut cfg.churn_diagonal_angle_deg,
                    15..=75,
                    "°",
                );
                ui.add_space(4.0);
                toggle_row(
                    ui,
                    p,
                    &mut self.show_churn_visualizer,
                    "Show Live Corner Visualizer on Screen",
                    "Displays a real-time overlay at physical screen corners showing the corner entry box, slide arrow, and angled corridor wedge",
                );
                if self.show_churn_visualizer {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Target Corner & Edge:")
                                .size(12.0)
                                .color(p.text_secondary),
                        );
                        let current_zone_name = match self.preview_zone {
                            Some(ChurnZoneId::TopRightDown) => {
                                "Right Edge — Top Corner → Down (Default)"
                            }
                            Some(ChurnZoneId::BottomRightUp) => "Right Edge — Bottom Corner → Up",
                            Some(ChurnZoneId::TopLeftDown) => "Left Edge — Top Corner → Down",
                            Some(ChurnZoneId::BottomLeftUp) => "Left Edge — Bottom Corner → Up",
                            Some(ChurnZoneId::TopLeftRight) => "Top Edge — Left Corner → Right",
                            Some(ChurnZoneId::TopRightLeft) => "Top Edge — Right Corner → Left",
                            Some(ChurnZoneId::BottomLeftRight) => {
                                "Bottom Edge — Left Corner → Right"
                            }
                            Some(ChurnZoneId::BottomRightLeft) => {
                                "Bottom Edge — Right Corner → Left"
                            }
                            None => "All Active Zones (Multi-Corner)",
                        };
                        egui::ComboBox::from_id_salt("churn_preview_zone_select")
                            .selected_text(current_zone_name)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.preview_zone,
                                    Some(ChurnZoneId::TopRightDown),
                                    "Right Edge — Top Corner → Down (Default)",
                                );
                                ui.selectable_value(
                                    &mut self.preview_zone,
                                    Some(ChurnZoneId::BottomRightUp),
                                    "Right Edge — Bottom Corner → Up",
                                );
                                ui.selectable_value(
                                    &mut self.preview_zone,
                                    Some(ChurnZoneId::TopLeftDown),
                                    "Left Edge — Top Corner → Down",
                                );
                                ui.selectable_value(
                                    &mut self.preview_zone,
                                    Some(ChurnZoneId::BottomLeftUp),
                                    "Left Edge — Bottom Corner → Up",
                                );
                                ui.selectable_value(
                                    &mut self.preview_zone,
                                    Some(ChurnZoneId::TopLeftRight),
                                    "Top Edge — Left Corner → Right",
                                );
                                ui.selectable_value(
                                    &mut self.preview_zone,
                                    Some(ChurnZoneId::TopRightLeft),
                                    "Top Edge — Right Corner → Left",
                                );
                                ui.selectable_value(
                                    &mut self.preview_zone,
                                    Some(ChurnZoneId::BottomLeftRight),
                                    "Bottom Edge — Left Corner → Right",
                                );
                                ui.selectable_value(
                                    &mut self.preview_zone,
                                    Some(ChurnZoneId::BottomRightLeft),
                                    "Bottom Edge — Right Corner → Left",
                                );
                                ui.selectable_value(
                                    &mut self.preview_zone,
                                    None,
                                    "All Active Zones (Multi-Corner)",
                                );
                            });
                    });
                }
                ui.add_space(4.0);
                slider_row(
                    ui,
                    p,
                    "Edge / Pill Hover Dwell (ms)",
                    "Hold duration before hovering an edge strip or pill opens the drawer",
                    &mut cfg.edge_dwell_ms,
                    0..=1000,
                    " ms",
                );
                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);
                toggle_row(
                    ui,
                    p,
                    &mut cfg.disable_in_fullscreen,
                    "Mute edge triggers during fullscreen games & videos",
                    "Checks SHQueryUserNotificationState and borderless fullscreen coverage",
                );
            },
        );

        ui.add_space(10.0);

        card(
            ui,
            p,
            "Area-Aware Edge Scrolling",
            Some(
                "Scroll anywhere along the thin screen edge trigger line to switch drawers (without aiming directly inside the pill)",
            ),
            |ui| {
                toggle_row(
                    ui,
                    p,
                    &mut cfg.area_aware_edge_scroll,
                    "Enable area-aware scroll on edge trigger line",
                    "Scrolling on the 4px translucent shaded edge line switches active stashed windows; once cursor enters the drawer body, the shade vanishes and scrolling controls the app",
                );
                ui.add_enabled_ui(cfg.area_aware_edge_scroll, |ui| {
                    ui.add_space(6.0);
                    let scroll_modes = [
                        (
                            AreaAwareScrollMode::OpenSessionStrict,
                            "Strict After Open Only (Recommended)",
                            "Active only once opened by the edge trigger and while your cursor stays inside the 4px shaded line before entering the app window.",
                        ),
                        (
                            AreaAwareScrollMode::InitialTriggerSession,
                            "Initial Trigger Session",
                            "Active from first touch/trigger along the 4px edge line until moving inward into the app window (hovering the pill always switches windows).",
                        ),
                        (
                            AreaAwareScrollMode::AlwaysOnTriggerLine,
                            "Always on Thin Trigger Line",
                            "Any time your cursor rests on the 4px edge line (even after returning from the app window), scrolling switches drawers.",
                        ),
                    ];
                    for (mode, title, desc) in scroll_modes {
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut cfg.area_aware_edge_scroll_mode, mode, "");
                            ui.vertical(|ui| {
                                ui.label(
                                    egui::RichText::new(title)
                                        .strong()
                                        .size(12.5)
                                        .color(p.text_primary),
                                );
                                ui.label(
                                    egui::RichText::new(desc)
                                        .size(11.0)
                                        .color(p.text_secondary),
                                );
                            });
                        });
                        ui.add_space(3.0);
                    }
                    ui.add_space(4.0);
                    toggle_row(
                        ui,
                        p,
                        &mut cfg.scroll_trigger_line_only_when_open,
                        "Active only while stashed windows are showing",
                        "Suppress the thin hairline indicator and scroll gestures whenever the stashed drawer is closed",
                    );
                });
            },
        );

        ui.add_space(10.0);

        card(
            ui,
            p,
            "Auto-Focus on Open & Focus Return",
            Some("Configure keyboard focus when revealing drawers and restoring focus on exit"),
            |ui| {
                toggle_row(
                    ui,
                    p,
                    &mut cfg.auto_focus_on_open,
                    "Auto-focus stashed window on open",
                    "Instantly gives keyboard focus to the stashed window (e.g. Messenger chat input) when summoned so you can start typing immediately",
                );
                toggle_row(
                    ui,
                    p,
                    &mut cfg.restore_prev_focus,
                    "Restore previous window focus on mouse leave",
                    "Returns keyboard focus back to your previous active window when mouse exits the stashed drawer",
                );
                toggle_row(
                    ui,
                    p,
                    &mut cfg.prioritize_attention_on_summon,
                    "Prioritize attention window on summon",
                    "Automatically opens the window requesting attention (e.g. new message) first when you trigger the edge",
                );
            },
        );

        ui.add_space(10.0);

        card(
            ui,
            p,
            "Churn Zones (Corner-Slide Bindings)",
            Some(
                "Assign each corner-slide direction to your last-opened stashed window, a specific app, or disable it",
            ),
            |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.set_max_width((ui.available_width() - 145.0).max(160.0));
                        ui.label(
                            egui::RichText::new(
                                "Dedicated app zones trigger only when a matching window is currently stashed (never falling back to another app), and do not overwrite your Default target.",
                            )
                            .size(11.0)
                            .color(p.text_secondary),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Refresh Open Apps").clicked() {
                            manual_refresh = true;
                        }
                    });
                });
                ui.add_space(6.0);

                for zone in ChurnZoneId::ALL {
                    let current = cfg.churn_zones.get(zone).clone();
                    let mode_idx = match &current {
                        ChurnZoneAction::DefaultLastOpened => 0_u8,
                        ChurnZoneAction::SpecificApp { .. } => 1_u8,
                        ChurnZoneAction::Disabled => 2_u8,
                    };
                    let mut next_mode = mode_idx;

                    Frame::NONE
                        .fill(p.bg_card_alt)
                        .stroke(Stroke::new(1.0_f32, p.border))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(10, 8))
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    egui::RichText::new(zone.label())
                                        .strong()
                                        .size(12.5)
                                        .color(p.text_primary),
                                );
                                if ui.small_button("👁 Preview").clicked() {
                                    self.show_churn_visualizer = true;
                                    self.preview_zone = Some(zone);
                                }
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.selectable_value(&mut next_mode, 2, "Disabled");
                                        ui.selectable_value(&mut next_mode, 1, "Specific App");
                                        ui.selectable_value(
                                            &mut next_mode,
                                            0,
                                            "Default (Last Opened)",
                                        );
                                    },
                                );
                            });

                            if next_mode != mode_idx {
                                let updated = match next_mode {
                                    1 => {
                                        let seed = app_candidates
                                            .first()
                                            .map(|(m, _)| m.clone())
                                            .unwrap_or_default();
                                        ChurnZoneAction::SpecificApp { matcher: seed }
                                    }
                                    2 => ChurnZoneAction::Disabled,
                                    _ => ChurnZoneAction::DefaultLastOpened,
                                };
                                cfg.churn_zones.set(zone, updated);
                            } else if let ChurnZoneAction::SpecificApp { mut matcher } = current {
                                ui.add_space(4.0);
                                let mut changed = false;
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(
                                        egui::RichText::new("App matcher (exe or title):")
                                            .size(11.5)
                                            .color(p.text_secondary),
                                    );
                                    let edit = ui.add(
                                        egui::TextEdit::singleline(&mut matcher)
                                            .hint_text("e.g. spotify.exe or Spotify")
                                            .desired_width(190.0),
                                    );
                                    if edit.changed() {
                                        changed = true;
                                    }

                                    egui::ComboBox::from_id_salt(("churn_zone_pick", zone))
                                        .selected_text("Pick running / stashed app...")
                                        .width(220.0)
                                        .show_ui(ui, |ui| {
                                            if app_candidates.is_empty() {
                                                ui.label(
                                                    egui::RichText::new(
                                                        "No open desktop apps detected",
                                                    )
                                                    .size(11.0)
                                                    .color(p.text_secondary),
                                                );
                                            }
                                            for (cand_matcher, cand_label) in &app_candidates {
                                                let selected =
                                                    matcher.eq_ignore_ascii_case(cand_matcher);
                                                if ui
                                                    .selectable_label(selected, cand_label)
                                                    .clicked()
                                                {
                                                    matcher = cand_matcher.clone();
                                                    changed = true;
                                                }
                                            }
                                        });
                                });
                                if changed {
                                    cfg.churn_zones
                                        .set(zone, ChurnZoneAction::SpecificApp { matcher });
                                }
                            }
                        });
                    ui.add_space(4.0);
                }
            },
        );

        ui.add_space(10.0);

        card(
            ui,
            p,
            "Window Drag & Drop Zones (Drop to Stash)",
            Some("Drag any standard desktop window to screen edges or corners to quickly stash it"),
            |ui| {
                toggle_row(
                    ui,
                    p,
                    &mut cfg.enable_drag_drop_zones,
                    "Enable Drag & Drop Stashing Zones",
                    "When moving a window near an edge, reveals translucent drop zones along the edge and corners for instant stashing",
                );
                if cfg.enable_drag_drop_zones {
                    ui.add_space(6.0);
                    toggle_row(
                        ui,
                        p,
                        &mut cfg.drag_drop_require_modifier,
                        "Require Hotkey (Shift + Drag)",
                        "Drop zones only appear when holding the hotkey while dragging a window, preventing any accidental triggers during regular window moves",
                    );
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            egui::RichText::new("Drag Hotkey:")
                                .size(12.5)
                                .color(p.text_primary),
                        );
                        ui.selectable_value(
                            &mut cfg.drag_drop_modifier_key,
                            DragDropModifierKey::Shift,
                            "Shift (Recommended)",
                        );
                        ui.selectable_value(
                            &mut cfg.drag_drop_modifier_key,
                            DragDropModifierKey::Ctrl,
                            "Ctrl",
                        );
                        ui.selectable_value(
                            &mut cfg.drag_drop_modifier_key,
                            DragDropModifierKey::Alt,
                            "Alt",
                        );
                        ui.selectable_value(
                            &mut cfg.drag_drop_modifier_key,
                            DragDropModifierKey::Any,
                            "Any Modifier",
                        );
                    });
                    ui.add_space(4.0);
                    slider_row(
                        ui,
                        p,
                        "Inward Offset (Avoid Win11 Snap)",
                        "Pushes drop zones inward towards screen center when using hotkey to avoid overlapping Windows 11 Snap layouts",
                        &mut cfg.drag_drop_inset_px,
                        0..=160,
                        " px",
                    );
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new("Active Screen Edges:")
                            .strong()
                            .size(12.5)
                            .color(p.text_primary),
                    );
                    ui.horizontal_wrapped(|ui| {
                        ui.checkbox(&mut cfg.drag_drop_left, "Left Edge");
                        ui.checkbox(&mut cfg.drag_drop_right, "Right Edge");
                        ui.checkbox(&mut cfg.drag_drop_top, "Top Edge (Disabled by default)");
                        ui.checkbox(&mut cfg.drag_drop_bottom, "Bottom Edge");
                    });
                    ui.add_space(6.0);
                    slider_row(
                        ui,
                        p,
                        "Edge Drop Zone Width",
                        "Hitbox width and visual preview depth for center edge stash zones",
                        &mut cfg.drag_drop_zone_px,
                        30..=200,
                        " px",
                    );
                    slider_row(
                        ui,
                        p,
                        "Corner Drop Zone Span",
                        "Square dimensions and reach of corner flick trigger drop zones",
                        &mut cfg.drag_corner_zone_px,
                        40..=200,
                        " px",
                    );
                    slider_row(
                        ui,
                        p,
                        "Drop Dwell Delay (ms)",
                        "Hold cursor inside zone for this duration to arm stashing (0 ms arms immediately)",
                        &mut cfg.drag_drop_dwell_ms,
                        0..=500,
                        " ms",
                    );
                }
            },
        );

        if manual_refresh {
            self.running_apps = enumerate_running_app_candidates();
            self.running_apps_refreshed_at = Instant::now();
        }
        self.sync_churn_visualizer_if_needed();
    }

    fn render_behavior_tab(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let cfg = &mut self.model.draft_config;
        card(
            ui,
            p,
            "Drawer Transition Animation",
            Some("Frame-locked to monitor refresh rate via DwmFlush()"),
            |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (style, label) in [
                        (AnimationStyle::Slide, "Slide From Bezel"),
                        (AnimationStyle::FloatAndFade, "Float & Fade"),
                        (AnimationStyle::Instant, "Instant (0 ms)"),
                    ] {
                        ui.selectable_value(&mut cfg.transition_style, style, label);
                    }
                });
                ui.add_space(6.0);
                slider_row(
                    ui,
                    p,
                    "Open Duration (ms)",
                    "Slide/fade speed when revealing a stashed drawer",
                    &mut cfg.open_duration_ms,
                    50..=500,
                    " ms",
                );
                slider_row(
                    ui,
                    p,
                    "Close Duration (ms)",
                    "Slide/fade speed when dismissing back to edge",
                    &mut cfg.close_duration_ms,
                    50..=500,
                    " ms",
                );
            },
        );

        ui.add_space(10.0);

        card(
            ui,
            p,
            "Edge Position Lock (Resize Only)",
            Some(
                "Keeps stashed windows docked to their screen edge instead of being dragged off their bezel",
            ),
            |ui| {
                toggle_row(
                    ui,
                    p,
                    &mut cfg.lock_stashed_position,
                    "Lock stashed window position (allow resize only)",
                    "Immediately snaps moved stashed windows back to their edge alignment while still allowing inner-edge thickness resizing",
                );
                ui.add_space(6.0);
                let warning_text = if cfg.lock_stashed_position {
                    "Warning: Keep this enabled. Letting users freely move stashed windows off their bezel breaks edge alignment, auto-hide triggers, and overlay header tracking."
                } else {
                    "WARNING: Stashed position lock is disabled! Moving a stashed window freely away from its edge will desynchronize triggers, auto-hide, and overlays — users should NOT be able to move stashed windows."
                };
                Frame::NONE
                    .fill(p.warning_soft)
                    .stroke(Stroke::new(1.0_f32, p.warning))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.label(
                            egui::RichText::new(warning_text)
                                .size(11.5)
                                .strong()
                                .color(p.warning),
                        );
                    });
            },
        );

        ui.add_space(10.0);

        card(
            ui,
            p,
            "Auto-Hide & Focus Restoration",
            Some("Hysteresis margin and popup menu guards remain active automatically"),
            |ui| {
                toggle_row(
                    ui,
                    p,
                    &mut cfg.auto_focus_on_open,
                    "Auto-focus stashed window on open",
                    "Instantly gives keyboard focus to the stashed window (e.g. Messenger chat input) when summoned so you can start typing immediately",
                );
                toggle_row(
                    ui,
                    p,
                    &mut cfg.hide_on_mouse_leave,
                    "Hide drawer when mouse leaves",
                    "Dismisses floating drawers after leaving bounds + 24px safety margin",
                );
                if cfg.hide_on_mouse_leave {
                    slider_row(
                        ui,
                        p,
                        "Mouse-Leave Delay (ms)",
                        "Grace countdown before closing after cursor exits",
                        &mut cfg.mouse_leave_delay_ms,
                        0..=5000,
                        " ms",
                    );
                }
                toggle_row(
                    ui,
                    p,
                    &mut cfg.hide_on_focus_loss,
                    "Hide immediately when clicking another window",
                    "Closes floating drawer as soon as another app gains foreground focus",
                );
                toggle_row(
                    ui,
                    p,
                    &mut cfg.restore_prev_focus,
                    "Restore previous window focus on mouse leave",
                    "Returns keyboard focus back to your previous active window when mouse exits the stashed drawer",
                );
                if cfg.restore_prev_focus {
                    slider_row(
                        ui,
                        p,
                        "Focus Restore Delay (ms)",
                        "Short pause before handing foreground focus back",
                        &mut cfg.focus_restore_delay_ms,
                        0..=2000,
                        " ms",
                    );
                }
            },
        );

        ui.add_space(10.0);

        card(
            ui,
            p,
            "Window Attention & Flashing Alerts",
            Some(
                "Configure notifications when stashed messaging or background apps call for attention",
            ),
            |ui| {
                toggle_row(
                    ui,
                    p,
                    &mut cfg.show_attention_pill,
                    "Show edge indicator pill when stashed window wants attention",
                    "Displays a pulsing amber pill with the app icon (e.g. Messenger) when a new message arrives while stashed",
                );
                toggle_row(
                    ui,
                    p,
                    &mut cfg.prioritize_attention_on_summon,
                    "Prioritize attention window on summon",
                    "Automatically opens the alerting window first when you bezel-push, corner-slide, or press a hotkey",
                );
            },
        );

        ui.add_space(10.0);

        card(
            ui,
            p,
            "Workspace & Virtual Desktop Defaults",
            Some("Defaults applied when stashing new windows"),
            |ui| {
                ui.horizontal(|ui| {
                    ui.label("Default Presentation Mode:");
                    ui.selectable_value(
                        &mut cfg.dock_mode,
                        DockMode::FloatingOverlay,
                        "Floating Overlay",
                    );
                    ui.selectable_value(
                        &mut cfg.dock_mode,
                        DockMode::FloatingPinned,
                        "Floating Pinned",
                    );
                    ui.selectable_value(
                        &mut cfg.dock_mode,
                        DockMode::NativeWorkAreaDock,
                        "Native Work-Area Dock",
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Default Virtual Desktop Scope:");
                    ui.selectable_value(
                        &mut cfg.virtual_desktop_mode,
                        VirtualDesktopMode::AllDesktops,
                        "Follow All Desktops",
                    );
                    ui.selectable_value(
                        &mut cfg.virtual_desktop_mode,
                        VirtualDesktopMode::CurrentDesktopOnly,
                        "Pin to Current Desktop",
                    );
                });
            },
        );
    }

    fn render_visuals_tab(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let cfg = &mut self.model.draft_config;
        card(
            ui,
            p,
            "Edge Indicator Pill & Backdrop Scrim",
            Some("Rendered hardware-accelerated with Direct2D & DirectComposition"),
            |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Edge Pill Style:");
                    ui.selectable_value(
                        &mut cfg.edge_pill_style,
                        EdgePillStyle::AppIconsStack,
                        "App Icons Stack",
                    );
                    ui.selectable_value(
                        &mut cfg.edge_pill_style,
                        EdgePillStyle::MinimalLine,
                        "Minimal Line",
                    );
                    ui.selectable_value(&mut cfg.edge_pill_style, EdgePillStyle::Hidden, "Hidden");
                });
                ui.add_space(6.0);
                toggle_row(
                    ui,
                    p,
                    &mut cfg.auto_hide_pill,
                    "Auto-hide Edge Pill",
                    "Only show the edge indicator pill while a drawer on that edge is open or when a stashed window closes",
                );
                toggle_row(
                    ui,
                    p,
                    &mut cfg.show_attention_pill,
                    "Show Attention Pill on Incoming Alert",
                    "Surfaces a pulsing amber pill with the app icon (e.g. Messenger) when a stashed window requests attention",
                );
                ui.add_space(6.0);
                slider_row(
                    ui,
                    p,
                    "Backdrop Dim (%)",
                    "Optional dark scrim behind open Floating Overlay drawers",
                    &mut cfg.dim_background,
                    0..=60,
                    "%",
                );
                slider_row(
                    ui,
                    p,
                    "Backdrop Dim Fade (ms)",
                    "Smooth fade-in/out duration for the backdrop scrim (0 ms = instant)",
                    &mut cfg.dim_fade_duration_ms,
                    0..=2000,
                    " ms",
                );
            },
        );

        ui.add_space(10.0);

        let pill_style = self.model.draft_config.edge_pill_style;
        let dim_pct = self.model.draft_config.dim_background;
        let area_scroll = self.model.draft_config.area_aware_edge_scroll;
        let auto_hide_pill = self.model.draft_config.auto_hide_pill;
        card(
            ui,
            p,
            "Live Preview",
            Some(if auto_hide_pill {
                "Approximate monitor bezel appearance (Auto-hide Pill active: shown when open or on close)"
            } else {
                "Approximate monitor bezel appearance"
            }),
            |ui| {
                let desired = Vec2::new(ui.available_width().min(540.0), 170.0);
                let (rect, _) = ui.allocate_exact_size(desired, egui::Sense::hover());
                let painter = ui.painter_at(rect);

                // Desktop background
                painter.rect_filled(rect, 10.0, Color32::from_rgb(20, 26, 38));
                painter.rect_stroke(
                    rect,
                    10.0,
                    Stroke::new(1.5_f32, p.border_strong),
                    egui::StrokeKind::Inside,
                );

                // Mock main app window underneath
                let main_win = egui::Rect::from_min_max(
                    egui::pos2(rect.left() + 20.0, rect.top() + 20.0),
                    egui::pos2(rect.right() - 150.0, rect.bottom() - 24.0),
                );
                painter.rect_filled(main_win, 6.0, Color32::from_rgb(35, 42, 58));

                // Dim scrim overlay
                if dim_pct > 0 {
                    let alpha = ((dim_pct as f32 / 100.0) * 255.0).round() as u8;
                    painter.rect_filled(rect, 10.0, Color32::from_black_alpha(alpha));
                }

                // 4px translucent area-scroll edge bezel indicator
                if area_scroll {
                    let shade = egui::Rect::from_min_max(
                        egui::pos2(rect.right() - 4.0, rect.top() + 10.0),
                        egui::pos2(rect.right(), rect.bottom() - 10.0),
                    );
                    painter.rect_filled(
                        shade,
                        2.0,
                        Color32::from_rgba_unmultiplied(76, 194, 255, 115),
                    );
                }

                // Sample edge-stashed drawer
                let drawer = egui::Rect::from_min_max(
                    egui::pos2(rect.right() - 138.0, rect.top() + 14.0),
                    egui::pos2(rect.right() - 10.0, rect.bottom() - 14.0),
                );
                painter.rect_filled(drawer, 6.0, p.bg_card_alt);
                painter.rect_stroke(
                    drawer,
                    6.0,
                    Stroke::new(1.0_f32, p.accent),
                    egui::StrokeKind::Inside,
                );

                // Edge Pill on right bezel
                match pill_style {
                    EdgePillStyle::Hidden => {}
                    EdgePillStyle::MinimalLine => {
                        let line = egui::Rect::from_center_size(
                            egui::pos2(rect.right() - 3.5, rect.center().y),
                            Vec2::new(4.0, 56.0),
                        );
                        painter.rect_filled(line, 2.0, p.accent);
                    }
                    EdgePillStyle::AppIconsStack => {
                        let pill = egui::Rect::from_center_size(
                            egui::pos2(rect.right() - 13.0, rect.center().y),
                            Vec2::new(22.0, 62.0),
                        );
                        painter.rect_filled(
                            pill,
                            8.0,
                            Color32::from_rgba_unmultiplied(18, 22, 32, 235),
                        );
                        painter.rect_stroke(
                            pill,
                            8.0,
                            Stroke::new(1.0_f32, p.accent),
                            egui::StrokeKind::Inside,
                        );
                        for dy in [-12.0, 12.0] {
                            painter.rect_filled(
                                egui::Rect::from_center_size(
                                    egui::pos2(pill.center().x, pill.center().y + dy),
                                    Vec2::new(12.0, 12.0),
                                ),
                                3.0,
                                p.accent,
                            );
                        }
                    }
                }
            },
        );
    }

    fn render_hotkeys_tab(&mut self, ui: &mut egui::Ui, p: &Palette) {
        self.model.validate_draft();

        card(
            ui,
            p,
            "Global Shortcut Bindings",
            Some("Type directly or click Record then press a key chord (Esc cancels recording)"),
            |ui| {
                for field in HotkeyField::ALL {
                    let is_recording = self.model.capturing_hotkey == Some(field);
                    let err_opt = self.model.hotkey_errors.get(&field).cloned();
                    let mut val = field.get(&self.model.draft_config.hotkeys).to_string();

                    Frame::NONE
                        .fill(p.bg_card_alt)
                        .stroke(Stroke::new(
                            1.0_f32,
                            if err_opt.is_some() {
                                p.error
                            } else if is_recording {
                                p.accent
                            } else {
                                p.border
                            },
                        ))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    ui.set_min_width(220.0);
                                    ui.label(
                                        egui::RichText::new(field.label())
                                            .strong()
                                            .size(13.0)
                                            .color(p.text_primary),
                                    );
                                    if let Some(err) = &err_opt {
                                        ui.label(
                                            egui::RichText::new(err).size(11.5).color(p.error),
                                        );
                                    } else if is_recording {
                                        ui.label(
                                            egui::RichText::new("Press desired key combo...")
                                                .size(11.5)
                                                .color(p.accent),
                                        );
                                    }
                                });

                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        let rec_lbl = if is_recording {
                                            "Cancel (Esc)"
                                        } else {
                                            "Record"
                                        };
                                        if ui.button(rec_lbl).clicked() {
                                            self.model.capturing_hotkey =
                                                if is_recording { None } else { Some(field) };
                                        }

                                        let edit = ui.add(
                                            egui::TextEdit::singleline(&mut val)
                                                .desired_width(185.0)
                                                .interactive(!is_recording),
                                        );
                                        if edit.changed() {
                                            field.set(&mut self.model.draft_config.hotkeys, val);
                                            self.model.validate_draft();
                                        }
                                    },
                                );
                            });
                        });
                    ui.add_space(4.0);
                }

                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "Supported tokens: Ctrl, Alt, Shift, Win + A-Z, 0-9, F1-F24, Left/Right/Up/Down, \\, Space, Tab, Enter, Esc.",
                    )
                    .size(11.5)
                    .color(p.text_secondary),
                );
            },
        );
    }
}

impl eframe::App for EdgeStashApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_worker_events();
        self.handle_hotkey_capture_input(ctx);

        let poll_interval = match self.model.active_tab {
            UiTab::Dashboard => Duration::from_millis(1200),
            _ => Duration::from_millis(2500),
        };
        if self.last_poll_at.elapsed() >= poll_interval {
            self.queue_poll();
        }
        if self.model.active_tab != UiTab::Triggers && self.show_churn_visualizer {
            self.show_churn_visualizer = false;
            self.sync_churn_visualizer_if_needed();
        }
        ctx.request_repaint_after(Duration::from_millis(750));

        let palette = apply_theme(ctx, self.theme_mode);
        self.render_top_header(ctx, &palette);
        self.render_bottom_bar(ctx, &palette);
        self.render_sidebar(ctx, &palette);
        self.render_content(ctx, &palette);
        self.render_save_preset_dialog(ctx, &palette);

        // Auto-apply settings changes debounced by 250ms during active user interaction/dragging,
        // or immediately flush if window loses focus.
        let is_focused = ctx.input(|i| i.focused);
        if self.model.capturing_hotkey.is_none()
            && self.model.is_dirty()
            && self.model.validate_draft()
        {
            if !is_focused {
                self.save_and_apply();
            } else {
                let now = Instant::now();
                match self.save_debounce_deadline {
                    None => {
                        self.save_debounce_deadline = Some(now + Duration::from_millis(250));
                        ctx.request_repaint_after(Duration::from_millis(250));
                    }
                    Some(deadline) if now >= deadline => {
                        self.save_debounce_deadline = None;
                        self.save_and_apply();
                    }
                    Some(deadline) => {
                        ctx.request_repaint_after(deadline.saturating_duration_since(now));
                    }
                }
            }
        } else {
            self.save_debounce_deadline = None;
        }
    }
}

impl Drop for EdgeStashApp {
    fn drop(&mut self) {
        if self.model.is_dirty() && self.model.validate_draft() {
            let _ = self.model.save_draft_to_disk();
            let client = PipeDaemonClient::new(DEFAULT_PIPE_NAME);
            let _ = client.send_request(&IpcRequest::ReloadConfig);
        }
        let _ = self
            .job_tx
            .send(WorkerJob::Action(IpcRequest::PreviewChurnVisual {
                enabled: false,
                zone: None,
                corner_zone_px: 0,
                slide_distance_px: 0,
                diagonal_angle_deg: 0,
            }));
    }
}

fn summary_kv(ui: &mut egui::Ui, p: &Palette, key: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(key).size(11.5).color(p.text_secondary));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(value)
                    .size(11.5)
                    .strong()
                    .color(p.text_primary),
            );
        });
    });
}

fn metric_tile(ui: &mut egui::Ui, p: &Palette, label: &str, value: &str, value_color: Color32) {
    Frame::NONE
        .fill(p.bg_card)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_min_width(140.0);
            ui.label(
                egui::RichText::new(label)
                    .size(11.5)
                    .color(p.text_secondary),
            );
            ui.label(
                egui::RichText::new(value)
                    .size(18.0)
                    .strong()
                    .color(value_color),
            );
        });
}

fn slider_row<Num: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    p: &Palette,
    title: &str,
    desc: &str,
    value: &mut Num,
    range: std::ops::RangeInclusive<Num>,
    suffix: &str,
) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_max_width((ui.available_width() - 290.0).max(140.0));
            ui.label(egui::RichText::new(title).size(13.5).color(p.text_primary));
            ui.label(egui::RichText::new(desc).size(11.5).color(p.text_secondary));
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add(egui::Slider::new(value, range).suffix(suffix));
        });
    });
}

fn gesture_label(mode: GestureMode) -> &'static str {
    match mode {
        GestureMode::EdgeBump => "Edge Bump",
        GestureMode::CornerSlide => "Corner Slide",
        GestureMode::EdgeHover => "Edge Hover",
        GestureMode::PillClick => "Pill Click",
        GestureMode::HotkeyOnly => "Hotkey Only",
    }
}

fn anim_label(style: AnimationStyle) -> &'static str {
    match style {
        AnimationStyle::Slide => "Slide",
        AnimationStyle::FloatAndFade => "Float & Fade",
        AnimationStyle::Instant => "Instant",
    }
}

fn dock_label(mode: DockMode) -> &'static str {
    match mode {
        DockMode::FloatingOverlay => "Floating Overlay",
        DockMode::FloatingPinned => "Floating Pinned",
        DockMode::NativeWorkAreaDock => "Native Dock",
    }
}

fn pill_label(style: EdgePillStyle) -> &'static str {
    match style {
        EdgePillStyle::AppIconsStack => "Icons Stack",
        EdgePillStyle::MinimalLine => "Minimal Line",
        EdgePillStyle::Hidden => "Hidden",
    }
}

fn edge_short(edge: ScreenEdge) -> &'static str {
    match edge {
        ScreenEdge::Left => "Left",
        ScreenEdge::Right => "Right",
        ScreenEdge::Top => "Top",
        ScreenEdge::Bottom => "Bottom",
    }
}

fn short_monitor_name(id: &str) -> &str {
    id.strip_prefix(r"\\.\").unwrap_or(id)
}

fn query_exe_basename(pid: u32) -> String {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::core::PWSTR;

    if pid == 0 {
        return String::new();
    }
    let Ok(proc_handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
    else {
        return String::new();
    };
    let mut buf = [0u16; 512];
    let mut len = buf.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(
            proc_handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok()
    };
    unsafe {
        let _ = CloseHandle(proc_handle);
    }
    if !ok || len == 0 {
        return String::new();
    }
    let full = String::from_utf16_lossy(&buf[..len as usize]);
    std::path::Path::new(&full)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&full)
        .to_string()
}

/// Enumerates visible desktop windows into deduplicated `(exe_matcher, display_label)` entries.
fn enumerate_running_app_candidates() -> Vec<(String, String)> {
    use std::collections::BTreeMap;
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
        IsWindowVisible,
    };

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() {
                return BOOL(1);
            }
            let len = GetWindowTextLengthW(hwnd);
            if len <= 0 {
                return BOOL(1);
            }
            let mut buf = vec![0u16; (len as usize) + 2];
            let copied = GetWindowTextW(hwnd, &mut buf);
            if copied <= 0 {
                return BOOL(1);
            }
            let title = String::from_utf16_lossy(&buf[..copied as usize])
                .trim()
                .to_string();
            if title.is_empty()
                || matches!(
                    title.as_str(),
                    "Program Manager" | "Default IME" | "MSCTFIME UI"
                )
            {
                return BOOL(1);
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            let exe = query_exe_basename(pid);
            if exe.is_empty() {
                return BOOL(1);
            }
            let lower = exe.to_ascii_lowercase();
            if matches!(lower.as_str(), "edgestash-ui.exe" | "edgestash-daemon.exe") {
                return BOOL(1);
            }
            let map = &mut *(lparam.0 as *mut BTreeMap<String, (String, String)>);
            map.entry(lower).or_insert((exe, title));
            BOOL(1)
        }
    }

    let mut found: BTreeMap<String, (String, String)> = BTreeMap::new();
    let ptr = (&mut found as *mut BTreeMap<String, (String, String)>) as isize;
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(ptr));
    }
    found
        .into_values()
        .map(|(exe, title)| (exe.clone(), format!("{title} ({exe})")))
        .collect()
}

fn build_churn_app_candidates(
    status: Option<&edgestash_ipc::protocol::DaemonStatusSnapshot>,
    running: &[(String, String)],
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    if let Some(st) = status {
        for slot in &st.slots {
            for win in &slot.windows {
                let matcher = if !win.process_name.trim().is_empty() {
                    win.process_name.trim().to_string()
                } else {
                    win.title.trim().to_string()
                };
                if matcher.is_empty() {
                    continue;
                }
                let key = matcher.to_ascii_lowercase();
                if seen.insert(key) {
                    let label = if win.title.is_empty() || matcher.eq_ignore_ascii_case(&win.title)
                    {
                        format!("[Stashed] {matcher}")
                    } else {
                        format!("[Stashed] {} ({matcher})", win.title)
                    };
                    out.push((matcher, label));
                }
            }
        }
    }

    for (matcher, label) in running {
        if seen.insert(matcher.to_ascii_lowercase()) {
            out.push((matcher.clone(), label.clone()));
        }
    }
    out
}

/// Resolves the default user configuration path used when launching `edgestash-ui`.
pub fn resolve_default_config_path() -> std::path::PathBuf {
    EdgeStashConfig::default_path().unwrap_or_else(|| std::path::PathBuf::from("edgestash.toml"))
}
