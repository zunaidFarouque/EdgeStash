use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use edgestash_ipc::config::{EdgeStashConfig, GestureMode, HotkeySpec};
use edgestash_ipc::config::ChurnZoneAction;
use edgestash_ipc::protocol::{
    DaemonStatusSnapshot, EdgeSlotSnapshot, IpcErrorCode, IpcRequest, IpcResponse,
    StashedWindowSnapshot,
};
use edgestash_ipc::types::{ChurnZoneId, DockMode, Rect, ScreenEdge, VirtualDesktopMode};
use edgestash_ui::hotkey::{
    HotkeyCaptureOutcome, HotkeyField, format_captured_hotkey, process_captured_key,
    validate_hotkey_config,
};
use edgestash_ui::ipc_bridge::{DaemonClient, resolve_daemon_binary_path};
use edgestash_ui::model::{BannerKind, DaemonConnection, EdgeStashUiModel};

struct MockDaemonClient {
    sent: Mutex<Vec<IpcRequest>>,
    responses: Mutex<VecDeque<io::Result<IpcResponse>>>,
}

impl MockDaemonClient {
    fn with_responses(responses: Vec<io::Result<IpcResponse>>) -> Self {
        Self {
            sent: Mutex::new(Vec::new()),
            responses: Mutex::new(VecDeque::from(responses)),
        }
    }

    fn offline() -> Self {
        Self::with_responses(vec![Err(io::Error::new(
            io::ErrorKind::NotFound,
            "pipe not found",
        ))])
    }

    fn take_sent(&self) -> Vec<IpcRequest> {
        self.sent.lock().unwrap().clone()
    }
}

impl DaemonClient for MockDaemonClient {
    fn send_request(&self, req: &IpcRequest) -> io::Result<IpcResponse> {
        self.sent.lock().unwrap().push(req.clone());
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(io::Error::new(io::ErrorKind::BrokenPipe, "no mock response")))
    }
}

fn temp_config_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "edgestash_ui_test_{name}_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("config.toml")
}

#[test]
fn model_loads_defaults_tracks_dirty_and_resets_or_discards() {
    let path = temp_config_path("dirty_reset");
    let mut model = EdgeStashUiModel::load_or_create(path.clone()).expect("load or create config");
    assert!(path.exists(), "load_or_create must write initial config.toml");
    assert!(!model.is_dirty());
    assert_eq!(model.draft_config.gesture_mode, GestureMode::EdgeBump);
    assert_eq!(model.draft_config.edge_bump_dwell_ms, 80);

    model.draft_config.gesture_mode = GestureMode::EdgeHover;
    model.draft_config.dim_background = 35;
    assert!(model.is_dirty());

    model.discard_changes();
    assert!(!model.is_dirty());
    assert_eq!(model.draft_config, EdgeStashConfig::default());

    model.draft_config.gesture_mode = GestureMode::PillClick;
    model.reset_to_defaults();
    assert_eq!(model.draft_config, EdgeStashConfig::default());

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}


#[test]
fn apply_and_save_clamps_validates_writes_toml_and_reloads_daemon_over_ipc() {
    let path = temp_config_path("save_reload");
    let mut model = EdgeStashUiModel::load_or_create(path.clone()).unwrap();

    model.draft_config.gesture_mode = GestureMode::EdgeHover;
    model.draft_config.corner_zone_px = 999;
    model.draft_config.dim_background = 120;
    model.draft_config.open_duration_ms = 5;

    let mock = MockDaemonClient::with_responses(vec![Ok(IpcResponse::Ok {
        message: "Acknowledged".to_string(),
    })]);
    model
        .apply_and_save(&mock)
        .expect("valid clamped config should save and reload");

    assert!(!model.is_dirty());
    assert_eq!(model.saved_config.corner_zone_px, 32);
    assert_eq!(model.saved_config.dim_background, 60);
    assert_eq!(model.saved_config.open_duration_ms, 50);
    assert_eq!(model.saved_config.gesture_mode, GestureMode::EdgeHover);

    let on_disk = EdgeStashConfig::load_or_create_at(&path).unwrap();
    assert_eq!(on_disk, model.saved_config);
    assert_eq!(mock.take_sent(), vec![IpcRequest::ReloadConfig]);
    assert_eq!(
        model.banner.as_ref().map(|b| b.kind),
        Some(BannerKind::Success)
    );

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}


#[test]
fn apply_and_save_rejects_invalid_hotkey_and_preserves_saved_file() {
    let path = temp_config_path("invalid_hotkey");
    let mut model = EdgeStashUiModel::load_or_create(path.clone()).unwrap();
    let original_raw = std::fs::read_to_string(&path).unwrap();

    model.draft_config.hotkeys.toggle_active = "Ctrl+Alt+NotAKey".to_string();
    let errors = validate_hotkey_config(&model.draft_config.hotkeys);
    assert!(errors.contains_key(&HotkeyField::ToggleActive));

    let mock = MockDaemonClient::with_responses(vec![]);
    let res = model.apply_and_save(&mock);
    assert!(res.is_err(), "invalid hotkey must block save");
    assert!(model.hotkey_errors.contains_key(&HotkeyField::ToggleActive));
    assert!(mock.take_sent().is_empty(), "must not send ReloadConfig");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        original_raw,
        "must not overwrite config.toml when invalid"
    );

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}


#[test]
fn apply_and_save_still_persists_when_daemon_is_offline() {
    let path = temp_config_path("offline_save");
    let mut model = EdgeStashUiModel::load_or_create(path.clone()).unwrap();
    model.draft_config.edge_dwell_ms = 240;

    let mock = MockDaemonClient::offline();
    model
        .apply_and_save(&mock)
        .expect("saving config should succeed even if daemon is offline");

    assert!(!model.is_dirty());
    assert_eq!(
        EdgeStashConfig::load_or_create_at(&path).unwrap().edge_dwell_ms,
        240
    );
    assert!(matches!(
        model.connection,
        DaemonConnection::Disconnected { .. }
    ));
    assert_eq!(
        model.banner.as_ref().map(|b| b.kind),
        Some(BannerKind::Warning)
    );

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}


#[test]
fn refresh_daemon_and_dispatch_live_slot_actions_update_snapshot() {
    let path = temp_config_path("live_slots");
    let mut model = EdgeStashUiModel::load_or_create(path.clone()).unwrap();

    let snapshot = DaemonStatusSnapshot {
        pid: 4242,
        slots: vec![EdgeSlotSnapshot {
            monitor_id: "DISPLAY1".to_string(),
            edge: ScreenEdge::Right,
            slot_index: 0,
            is_active_in_deck: true,
            is_open: false,
            needs_attention: false,
            dock_mode: DockMode::FloatingOverlay,
            virtual_desktop_mode: VirtualDesktopMode::AllDesktops,
            windows: vec![StashedWindowSnapshot {
                hwnd: 0x1234,
                title: "Notes.txt".to_string(),
                process_name: "notepad.exe".to_string(),
                target_rect: Rect::new(1320, 0, 1920, 1040),
                needs_attention: false,
            }],
        }],
    };
    let mut opened_snapshot = snapshot.clone();
    opened_snapshot.slots[0].is_open = true;

    let mock = MockDaemonClient::with_responses(vec![
        Ok(IpcResponse::Pong {
            version: "0.1.0".to_string(),
            pid: 4242,
        }),
        Ok(IpcResponse::Status(snapshot.clone())),
        Ok(IpcResponse::Ok {
            message: "Toggled".to_string(),
        }),
        Ok(IpcResponse::Status(opened_snapshot.clone())),
        Ok(IpcResponse::Error {
            code: IpcErrorCode::NoSlotOnEdge,
            message: "No slot".to_string(),
        }),
    ]);

    model.refresh_daemon(&mock);
    assert_eq!(
        model.connection,
        DaemonConnection::Connected {
            version: "0.1.0".to_string(),
            pid: 4242,
        }
    );
    assert_eq!(model.status_snapshot.as_ref(), Some(&snapshot));

    let toggle_req = IpcRequest::ToggleEdge {
        monitor_id: Some("DISPLAY1".to_string()),
        edge: ScreenEdge::Right,
    };
    model.dispatch_action(&mock, toggle_req.clone());
    assert_eq!(model.status_snapshot.as_ref(), Some(&opened_snapshot));
    assert_eq!(
        model.banner.as_ref().map(|b| b.kind),
        Some(BannerKind::Success)
    );

    model.dispatch_action(&mock, IpcRequest::UnstashAll);
    assert_eq!(
        model.banner.as_ref().map(|b| b.kind),
        Some(BannerKind::Error)
    );
    assert_eq!(
        mock.take_sent(),
        vec![
            IpcRequest::Ping,
            IpcRequest::Status,
            toggle_req,
            IpcRequest::Status,
            IpcRequest::UnstashAll,
        ]
    );

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}


#[test]
fn hotkey_capture_formats_modifiers_and_primary_keys_roundtrip_with_ipc_parser() {
    let mods = egui::Modifiers {
        alt: true,
        ctrl: true,
        shift: false,
        mac_cmd: false,
        command: false,
    };
    let captured = format_captured_hotkey(mods, egui::Key::Backslash)
        .expect("Ctrl+Alt+Backslash must format");
    assert_eq!(captured, "Ctrl+Alt+\\");
    assert!(HotkeySpec::parse(&captured).is_ok());

    let arrow = format_captured_hotkey(
        egui::Modifiers {
            alt: true,
            ctrl: true,
            shift: true,
            ..Default::default()
        },
        egui::Key::ArrowLeft,
    )
    .expect("arrow hotkey must format");
    assert_eq!(arrow, "Ctrl+Alt+Shift+Left");
    assert!(HotkeySpec::parse(&arrow).is_ok());

    assert_eq!(
        process_captured_key(egui::Modifiers::NONE, egui::Key::Escape),
        HotkeyCaptureOutcome::Cancelled
    );
    assert_eq!(
        process_captured_key(egui::Modifiers::CTRL, egui::Key::Space),
        HotkeyCaptureOutcome::Captured("Ctrl+Space".to_string())
    );
}

#[test]
fn resolve_daemon_binary_finds_sibling_or_fallback() {
    let resolved = resolve_daemon_binary_path(Some(Path::new(r"C:\Tools\EdgeStash\edgestash-ui.exe")));
    assert_eq!(
        resolved,
        PathBuf::from(r"C:\Tools\EdgeStash\edgestash-daemon.exe")
    );
    assert_eq!(
        resolve_daemon_binary_path(None),
        PathBuf::from("edgestash-daemon.exe")
    );
}

#[test]
fn ui_model_validates_dim_fade_area_aware_scroll_and_floating_pinned_defaults() {
    let path = temp_config_path("new_settings");
    let mut model = EdgeStashUiModel::load_or_create(path.clone()).unwrap();

    assert_eq!(model.draft_config.dim_fade_duration_ms, 160);
    assert!(model.draft_config.area_aware_edge_scroll);
    assert_eq!(
        model.draft_config.area_aware_edge_scroll_mode,
        edgestash_ipc::config::AreaAwareScrollMode::OpenSessionStrict
    );
    assert!(model.draft_config.lock_stashed_position);
    assert!(!model.draft_config.auto_hide_pill);

    model.draft_config.dim_fade_duration_ms = 5000;
    model.draft_config.area_aware_edge_scroll = false;
    model.draft_config.area_aware_edge_scroll_mode =
        edgestash_ipc::config::AreaAwareScrollMode::AlwaysOnTriggerLine;
    model.draft_config.dock_mode = DockMode::FloatingPinned;
    model.draft_config.lock_stashed_position = false;
    model.draft_config.auto_hide_pill = true;
    model.draft_config.churn_zones.set(
        ChurnZoneId::BottomRightUp,
        ChurnZoneAction::SpecificApp {
            matcher: "music".to_string(),
        },
    );
    assert!(model.is_dirty());

    model.save_draft_to_disk().expect("save draft clamps dim fade");
    assert_eq!(model.saved_config.dim_fade_duration_ms, 2000);
    assert_eq!(model.saved_config.dock_mode, DockMode::FloatingPinned);
    assert!(!model.saved_config.lock_stashed_position);
    assert!(model.saved_config.auto_hide_pill);
    assert_eq!(
        model.saved_config.churn_zones.get(ChurnZoneId::BottomRightUp),
        &ChurnZoneAction::SpecificApp {
            matcher: "music".to_string()
        }
    );
    assert!(!model.is_dirty());

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn save_and_load_named_preset_generates_actual_config_file() {
    let base_cfg_path = temp_config_path("test-named-preset-base");
    let mut model = EdgeStashUiModel::load_or_create(base_cfg_path.clone()).unwrap();

    // Customize draft settings
    model.draft_config.drag_drop_zone_px = 110;
    model.draft_config.drag_corner_zone_px = 125;
    model.draft_config.drag_drop_require_modifier = true;
    model.draft_config.drag_drop_inset_px = 55;
    model.draft_config.drag_drop_top = true;

    // 1. Validation rejects invalid preset names
    assert!(model.save_as_named_preset("   ").is_err(), "Empty preset name must fail");
    assert!(model.save_as_named_preset("Bad/Name").is_err(), "Invalid path chars must fail");
    assert!(model.save_as_named_preset("Bad:Name").is_err(), "Invalid colon must fail");

    // 2. Saving as named preset generates the actual file on disk
    let preset_name = "Test_Workspace_Preset_999";
    let saved_path = model
        .save_as_named_preset(preset_name)
        .expect("Named preset should save successfully");

    assert!(saved_path.is_file(), "Generated preset config file must exist on disk");
    assert!(saved_path.ends_with(format!("{}.toml", preset_name)));

    // 3. Inspect generated file content
    let content = std::fs::read_to_string(&saved_path).expect("Read preset file");
    let parsed = EdgeStashConfig::from_toml_str(&content).expect("Parse generated preset toml");
    assert_eq!(parsed.drag_drop_zone_px, 110);
    assert_eq!(parsed.drag_corner_zone_px, 125);
    assert!(parsed.drag_drop_require_modifier);
    assert_eq!(parsed.drag_drop_inset_px, 55);
    assert!(parsed.drag_drop_top);

    // 4. Loading the preset from disk restores all settings into another model
    let other_path = temp_config_path("test-named-preset-other");
    let mut other_model = EdgeStashUiModel::load_or_create(other_path.clone()).unwrap();
    assert_ne!(other_model.draft_config.drag_drop_zone_px, 110);

    other_model
        .load_preset_from_disk(&saved_path)
        .expect("Load preset should succeed");
    assert_eq!(other_model.draft_config.drag_drop_zone_px, 110);
    assert_eq!(other_model.draft_config.drag_corner_zone_px, 125);
    assert!(other_model.draft_config.drag_drop_require_modifier);
    assert_eq!(other_model.draft_config.drag_drop_inset_px, 55);
    assert!(other_model.draft_config.drag_drop_top);

    // Cleanup
    let _ = std::fs::remove_file(saved_path);
    let _ = std::fs::remove_dir_all(base_cfg_path.parent().unwrap());
    let _ = std::fs::remove_dir_all(other_path.parent().unwrap());
}
