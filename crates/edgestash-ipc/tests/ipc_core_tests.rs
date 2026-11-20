use std::io::Cursor;

use edgestash_ipc::config::{
    AreaAwareScrollMode, ChurnZoneAction, EdgeStashConfig, GestureMode, HotkeySpec, VirtualKey,
};
use edgestash_ipc::protocol::{
    IpcErrorCode, IpcRequest, IpcResponse, MAX_IPC_FRAME_BYTES, decode_frame, encode_frame,
    read_frame, write_frame,
};
use edgestash_ipc::types::{
    AnimationStyle, ChurnZoneId, DockMode, EdgePillStyle, Rect, ScreenEdge, VirtualDesktopMode,
};

#[test]
fn config_defaults_match_master_specification() {
    let cfg = EdgeStashConfig::default();

    assert_eq!(cfg.gesture_mode, GestureMode::EdgeBump);
    assert_eq!(cfg.edge_bump_dwell_ms, 80);
    assert_eq!(cfg.corner_zone_px, 8);
    assert_eq!(cfg.slide_distance_px, 40);
    assert_eq!(cfg.corner_timeout_ms, 1000);
    assert_eq!(cfg.edge_dwell_ms, 120);
    assert!(cfg.disable_in_fullscreen);
    assert_eq!(cfg.transition_style, AnimationStyle::Slide);
    assert_eq!(cfg.open_duration_ms, 160);
    assert_eq!(cfg.close_duration_ms, 130);
    assert!(cfg.auto_focus_on_open);
    assert!(cfg.hide_on_mouse_leave);
    assert_eq!(cfg.mouse_leave_delay_ms, 350);
    assert!(cfg.hide_on_focus_loss);
    assert!(cfg.restore_prev_focus);
    assert_eq!(cfg.focus_restore_delay_ms, 50);
    assert_eq!(cfg.dock_mode, DockMode::FloatingOverlay);
    assert_eq!(cfg.virtual_desktop_mode, VirtualDesktopMode::AllDesktops);
    assert_eq!(cfg.edge_pill_style, EdgePillStyle::AppIconsStack);
    assert_eq!(cfg.dim_background, 0);
    assert_eq!(cfg.churn_diagonal_angle_deg, 45);

    // Conflict-free default hotkeys
    assert_eq!(cfg.hotkeys.stash_left, "Ctrl+Alt+Shift+Left");
    assert_eq!(cfg.hotkeys.stash_right, "Ctrl+Alt+Shift+Right");
    assert_eq!(cfg.hotkeys.stash_top, "Ctrl+Alt+Shift+Up");
    assert_eq!(cfg.hotkeys.stash_bottom, "Ctrl+Alt+Shift+Down");
    assert_eq!(cfg.hotkeys.toggle_active, "Ctrl+Alt+\\");
    assert_eq!(cfg.hotkeys.group_active, "Ctrl+Alt+G");
    assert_eq!(cfg.hotkeys.unstash_active, "Ctrl+Alt+U");
}

#[test]
fn partial_toml_uses_defaults_and_clamps_out_of_range_values() {
    let raw = r#"
        gesture_mode = "EdgeHover"
        corner_zone_px = 999
        edge_dwell_ms = 5000
        open_duration_ms = 10
        close_duration_ms = 900
        mouse_leave_delay_ms = 99999
        focus_restore_delay_ms = 5000
        dim_background = 95
        churn_diagonal_angle_deg = 180
        dock_mode = "NativeWorkAreaDock"
    "#;

    let cfg = EdgeStashConfig::from_toml_str(raw).expect("valid partial TOML should parse");
    assert_eq!(cfg.gesture_mode, GestureMode::EdgeHover);
    assert_eq!(cfg.corner_zone_px, 32);
    assert_eq!(cfg.edge_dwell_ms, 1000);
    assert_eq!(cfg.open_duration_ms, 50);
    assert_eq!(cfg.close_duration_ms, 500);
    assert_eq!(cfg.mouse_leave_delay_ms, 5000);
    assert_eq!(cfg.focus_restore_delay_ms, 2000);
    assert_eq!(cfg.dim_background, 60);
    assert_eq!(cfg.churn_diagonal_angle_deg, 75);
    assert_eq!(cfg.dock_mode, DockMode::NativeWorkAreaDock);
    // Unspecified fields keep defaults
    assert_eq!(cfg.edge_pill_style, EdgePillStyle::AppIconsStack);
    assert!(cfg.disable_in_fullscreen);
}

#[test]
fn hotkey_spec_parses_valid_and_rejects_invalid_bindings() {
    let left = HotkeySpec::parse("Ctrl+Alt+Shift+Left").expect("valid hotkey");
    assert!(left.ctrl && left.alt && left.shift && !left.win);
    assert_eq!(left.key, VirtualKey::Left);
    assert_eq!(left.vk_code(), 0x25);

    let backslash = HotkeySpec::parse("Ctrl+Alt+\\").expect("backslash hotkey");
    assert!(backslash.ctrl && backslash.alt && !backslash.shift && !backslash.win);
    assert_eq!(backslash.key, VirtualKey::Backslash);
    assert_eq!(backslash.vk_code(), 0xDC);

    let win_arrow = HotkeySpec::parse("Win+Shift+Right").expect("win+shift+right");
    assert!(win_arrow.win && win_arrow.shift && !win_arrow.ctrl && !win_arrow.alt);
    assert_eq!(win_arrow.vk_code(), 0x27);

    let letter = HotkeySpec::parse("ctrl + alt + g").expect("case-insensitive hotkey");
    assert!(letter.ctrl && letter.alt);
    assert_eq!(letter.vk_code(), b'G' as u32);

    assert!(HotkeySpec::parse("").is_err());
    assert!(HotkeySpec::parse("Ctrl+Alt").is_err());
    assert!(HotkeySpec::parse("Ctrl+BogusKey").is_err());
}

#[test]
fn ipc_frame_roundtrip_and_limits() {
    let req = IpcRequest::StashForeground {
        edge: ScreenEdge::Right,
        group_with_active: true,
    };
    let bytes = encode_frame(&req).expect("encode request");
    let decoded: IpcRequest = decode_frame(&bytes).expect("decode request");
    assert_eq!(decoded, req);

    let preview_req = IpcRequest::PreviewChurnVisual {
        enabled: true,
        zone: Some(ChurnZoneId::TopRightDown),
        corner_zone_px: 24,
        slide_distance_px: 60,
        diagonal_angle_deg: 50,
    };
    let preview_bytes = encode_frame(&preview_req).expect("encode preview request");
    let preview_decoded: IpcRequest = decode_frame(&preview_bytes).expect("decode preview request");
    assert_eq!(preview_decoded, preview_req);

    let resp = IpcResponse::Error {
        code: IpcErrorCode::WindowProtected,
        message: "Cannot stash shell window".to_string(),
    };
    let mut buf = Vec::new();
    write_frame(&mut buf, &resp).expect("write frame");
    let mut cursor = Cursor::new(buf);
    let read_back: IpcResponse = read_frame(&mut cursor).expect("read frame");
    assert_eq!(read_back, resp);

    // Oversized frame header must fail cleanly without panicking or allocating
    let mut bad_header = ((MAX_IPC_FRAME_BYTES as u32) + 1).to_le_bytes().to_vec();
    bad_header.extend_from_slice(b"{}");
    let mut bad_cursor = Cursor::new(bad_header);
    assert!(read_frame::<_, IpcRequest>(&mut bad_cursor).is_err());
}

#[test]
fn rect_geometry_helpers_work() {
    let r = Rect::new(100, 50, 500, 850);
    assert_eq!(r.width(), 400);
    assert_eq!(r.height(), 800);
    assert!(r.contains(100, 50));
    assert!(r.contains(499, 849));
    assert!(!r.contains(500, 850));

    let expanded = r.expanded(24);
    assert_eq!(expanded, Rect::new(76, 26, 524, 874));
    assert!(expanded.contains_inclusive(76, 26));
    assert!(expanded.contains_inclusive(524, 874));
}

#[test]
fn config_file_load_or_create_and_vdesktop_guid_roundtrip() {
    use edgestash_ipc::types::VirtualDesktopScope;
    use windows::core::GUID;

    let temp_dir = std::env::temp_dir().join(format!("edgestash-ipc-test-{}", std::process::id()));
    let cfg_path = temp_dir.join("config.toml");
    let _ = std::fs::remove_dir_all(&temp_dir);

    let created =
        EdgeStashConfig::load_or_create_at(&cfg_path).expect("should create default config");
    assert_eq!(created, EdgeStashConfig::default());
    assert!(cfg_path.exists());

    let loaded =
        EdgeStashConfig::load_or_create_at(&cfg_path).expect("should load existing config");
    assert_eq!(loaded, created);

    let mut modified = loaded.clone();
    modified.gesture_mode = GestureMode::EdgeHover;
    modified
        .save_to_at(&cfg_path)
        .expect("save modified config");
    let reloaded = EdgeStashConfig::load_or_create_at(&cfg_path).expect("reload saved config");
    assert_eq!(reloaded.gesture_mode, GestureMode::EdgeHover);
    let _ = std::fs::remove_dir_all(&temp_dir);

    let guid = GUID::from_u128(0x12345678_9abc_def0_1122_334455667788);
    let scope = VirtualDesktopScope::from_guid(guid);
    assert_eq!(scope.as_guid(), Some(guid));

    let spec = HotkeySpec::parse("Ctrl+Alt+Shift+Left").unwrap();
    assert_eq!(spec.win32_modifiers(), 0x4000 | 0x0001 | 0x0002 | 0x0004);
}

#[test]
fn config_defaults_and_clamping_cover_dim_fade_floating_pinned_and_area_aware_scroll() {
    let cfg = EdgeStashConfig::default();
    assert_eq!(cfg.dim_fade_duration_ms, 160);
    assert!(cfg.area_aware_edge_scroll);
    assert_eq!(
        cfg.area_aware_edge_scroll_mode,
        AreaAwareScrollMode::OpenSessionStrict
    );
    assert!(cfg.lock_stashed_position);
    assert!(!cfg.auto_hide_pill);
    for zone in ChurnZoneId::ALL {
        assert_eq!(
            cfg.churn_zones.get(zone),
            &ChurnZoneAction::DefaultLastOpened
        );
    }

    // Existing partial TOML without new fields preserves exact defaults cleanly
    let legacy = r#"
        gesture_mode = "EdgeHover"
        dim_background = 25
    "#;
    let parsed_legacy = EdgeStashConfig::from_toml_str(legacy).expect("legacy TOML parses");
    assert_eq!(parsed_legacy.dim_fade_duration_ms, 160);
    assert!(parsed_legacy.area_aware_edge_scroll);
    assert_eq!(
        parsed_legacy.area_aware_edge_scroll_mode,
        AreaAwareScrollMode::OpenSessionStrict
    );
    assert!(parsed_legacy.lock_stashed_position);
    assert!(!parsed_legacy.auto_hide_pill);

    let custom = r#"
        dock_mode = "FloatingPinned"
        dim_fade_duration_ms = 99999
        area_aware_edge_scroll = false
        area_aware_edge_scroll_mode = "AlwaysOnTriggerLine"
        lock_stashed_position = false
        auto_hide_pill = true

        [churn_zones]
        bottom_right_up = { type = "SpecificApp", matcher = "spotify.exe" }
        top_left_down = { type = "Disabled" }
    "#;
    let parsed_custom = EdgeStashConfig::from_toml_str(custom).expect("custom TOML parses");
    assert_eq!(parsed_custom.dock_mode, DockMode::FloatingPinned);
    assert_eq!(parsed_custom.dim_fade_duration_ms, 2000);
    assert!(!parsed_custom.area_aware_edge_scroll);
    assert_eq!(
        parsed_custom.area_aware_edge_scroll_mode,
        AreaAwareScrollMode::AlwaysOnTriggerLine
    );
    assert!(!parsed_custom.lock_stashed_position);
    assert!(parsed_custom.auto_hide_pill);
    assert_eq!(
        parsed_custom.churn_zones.get(ChurnZoneId::BottomRightUp),
        &ChurnZoneAction::SpecificApp {
            matcher: "spotify.exe".to_string()
        }
    );
    assert_eq!(
        parsed_custom.churn_zones.get(ChurnZoneId::TopLeftDown),
        &ChurnZoneAction::Disabled
    );
    assert_eq!(
        parsed_custom.churn_zones.get(ChurnZoneId::TopRightDown),
        &ChurnZoneAction::DefaultLastOpened
    );

    let req = IpcRequest::SetDockMode {
        monitor_id: None,
        edge: ScreenEdge::Right,
        mode: DockMode::FloatingPinned,
    };
    let roundtrip: IpcRequest = decode_frame(&encode_frame(&req).unwrap()).unwrap();
    assert_eq!(roundtrip, req);
}
