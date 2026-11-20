use edgestash_daemon::crash_recovery::{CrashManifest, CrashRecoveryEntry, recover_orphans_with};
use edgestash_daemon::focus::{DismissalAction, DismissalController};
use edgestash_daemon::platform::win32::is_non_fullscreen_shell_or_internal_class;
use edgestash_daemon::platform::{Clock, FakeClock, MonitorInfo, WindowHandle};
use edgestash_daemon::renderer::DimScrimFade;
use edgestash_daemon::trigger::{
    AreaAwareScrollController, CharmsStateMachine, DirectedCharmsDetector, DragCaptureStateMachine,
    DragDropTarget, EdgeBumpStateMachine, EdgeHoverStateMachine, StripScrollDecision,
    allows_pill_click, allows_pill_hover, allows_strip_hover, area_scroll_shade_rect,
    compute_churn_preview_rect, compute_dim_scrim_insert_after, compute_drop_preview_rect,
    compute_edge_band_rect, compute_edge_band_rect_inset, corner_candidate_edges,
    detect_drag_drop_zone, detect_drag_drop_zone_ext, detect_near_edge, detect_near_edge_ext,
    edge_pill_rect, edge_pill_rect_for_style, hit_test_pill_slot_index, is_strip_input_transparent,
    should_pill_be_visible,
};
use edgestash_ipc::config::{
    AreaAwareScrollMode, DragDropModifierKey, EdgeStashConfig, GestureMode,
};
use edgestash_ipc::types::{ChurnZoneId, DockMode, EdgePillStyle, Rect, ScreenEdge};

fn mon() -> MonitorInfo {
    MonitorInfo {
        id: "MON-1".to_string(),
        rect: Rect::new(0, 0, 1920, 1080),
        work_rect: Rect::new(0, 0, 1920, 1032),
        dpi: 96,
        is_primary: true,
    }
}

#[test]
fn charms_corner_slide_triggers_only_on_bezel_slide_within_timeout() {
    let clock = FakeClock::new(1000);
    let mut charms = CharmsStateMachine::new(8, 40, 600);
    let m = mon();

    // Moving near top-right close button [1915, 10] then moving inward (not down bezel) must NOT trigger
    assert!(!charms.update(&m, ScreenEdge::Right, (1915, 4), clock.now_ms()));
    clock.advance(100);
    assert!(!charms.update(&m, ScreenEdge::Right, (1850, 50), clock.now_ms()));

    // Hit top-right 8x8 corner [1916, 2], slide down along bezel to [1918, 48] within 200ms -> triggers!
    assert!(!charms.update(&m, ScreenEdge::Right, (1916, 2), clock.now_ms()));
    clock.advance(200);
    assert!(charms.update(&m, ScreenEdge::Right, (1918, 48), clock.now_ms()));

    // Staying on edge doesn't double-trigger until cursor leaves bezel corridor
    clock.advance(50);
    assert!(!charms.update(&m, ScreenEdge::Right, (1918, 80), clock.now_ms()));

    // Timeout (> 600ms) prevents trigger
    charms.reset();
    assert!(!charms.update(&m, ScreenEdge::Left, (2, 2), clock.now_ms()));
    clock.advance(650);
    assert!(!charms.update(&m, ScreenEdge::Left, (2, 60), clock.now_ms()));
}

#[test]
fn edge_hover_requires_full_dwell_and_resets_on_exit() {
    let mut hover = EdgeHoverStateMachine::new(120);
    assert!(!hover.update(true, 100));
    assert!(!hover.update(true, 219));
    // Leaves before 120ms elapsed
    assert!(!hover.update(false, 219));

    // Re-enters at t=300 -> must wait full 120ms until 420
    assert!(!hover.update(true, 300));
    assert!(!hover.update(true, 419));
    assert!(hover.update(true, 420));
    // Does not re-fire while hovering continuously
    assert!(!hover.update(true, 500));
}

#[test]
fn edge_bump_state_machine_triggers_after_80ms_dwell_on_bezel() {
    let mut bump = EdgeBumpStateMachine::new(80);
    let m = mon();

    // Mouse outside initial hit zone (e.g. 1910 on 1920 width, > 4px from bezel) -> not armed
    assert!(!bump.update(&m, ScreenEdge::Right, (1910, 500), 1000));
    assert!(!bump.is_armed());

    // Mouse touches bezel (1919, 500) -> armed!
    assert!(!bump.update(&m, ScreenEdge::Right, (1919, 500), 1050));
    assert!(bump.is_armed());

    // Moving away into screen (> 12px away) resets
    assert!(!bump.update(&m, ScreenEdge::Right, (1900, 500), 1100));
    assert!(!bump.is_armed());

    // Touches bezel again at 1200ms
    assert!(!bump.update(&m, ScreenEdge::Right, (1919, 500), 1200));
    assert!(bump.is_armed());

    // At 1279ms (79ms) -> not triggered yet
    assert!(!bump.update(&m, ScreenEdge::Right, (1919, 510), 1279));

    // At 1280ms (80ms) -> triggers!
    assert!(bump.update(&m, ScreenEdge::Right, (1919, 510), 1280));

    // Does not fire repeatedly while staying on bezel
    assert!(!bump.update(&m, ScreenEdge::Right, (1919, 520), 1350));

    // Once user leaves bezel (> 12px), re-arms on next touch
    assert!(!bump.update(&m, ScreenEdge::Right, (1900, 520), 1400));
    assert!(!bump.is_armed());
}

#[test]
fn edge_bump_state_machine_maintains_dwell_while_moving_in_push_band() {
    let mut bump = EdgeBumpStateMachine::new(80);
    let m = mon();

    // Touches bezel at x=1918 (within 4px hit zone on 1920 width)
    assert!(!bump.update(&m, ScreenEdge::Right, (1918, 500), 1000));
    assert!(bump.is_armed());

    // Moving while pushing: x moves to 1914, 1911, 1915 (within 12px push band)
    assert!(!bump.update(&m, ScreenEdge::Right, (1914, 510), 1030));
    assert!(
        bump.is_armed(),
        "Micro-jitter within 12px push band must not abort dwell!"
    );

    assert!(!bump.update(&m, ScreenEdge::Right, (1911, 520), 1060));
    assert!(bump.is_armed(), "Still in push band, dwell continues!");

    // At 1080ms (80ms total), triggers even while moving!
    assert!(bump.update(&m, ScreenEdge::Right, (1915, 530), 1080));

    // Staying in push band doesn't re-trigger
    assert!(!bump.update(&m, ScreenEdge::Right, (1916, 540), 1100));

    // Pulling away into desktop (x = 1900, 20px > 12px) resets to Outside
    assert!(!bump.update(&m, ScreenEdge::Right, (1900, 540), 1150));
    assert!(!bump.is_armed());
}

#[test]
fn directed_charms_arms_in_32px_corner_and_rearms_on_corner_return() {
    let m = mon();
    let mut detector = DirectedCharmsDetector::new(8, 40, 1000);

    // Natural sweep into top-right corner: x=1900 (20px from right), y=15 (15px from top)
    // Must arm! (Not be rejected by an overly strict 8px box)
    assert!(detector.update(&m, (1900, 15), 1000).is_empty());
    assert!(
        detector.is_any_armed(),
        "32px corner box must arm natural sweeps!"
    );

    // Slide down to y=65 along right edge -> triggers TopRightDown
    let fired = detector.update(&m, (1905, 65), 1100);
    assert_eq!(fired, vec![ChurnZoneId::TopRightDown]);

    // Move back UP into the top-right corner along the bezel (without moving into center of screen)
    let re_fired = detector.update(&m, (1910, 10), 1200);
    assert!(re_fired.is_empty());
    assert!(
        detector.is_any_armed(),
        "Returning to corner must immediately re-arm without lockout!"
    );

    // Slide down again -> triggers again!
    let fired_again = detector.update(&m, (1908, 65), 1300);
    assert_eq!(fired_again, vec![ChurnZoneId::TopRightDown]);
}

#[test]
fn directed_charms_triggers_at_absolute_edge_and_rejects_wide_diagonal_sweep() {
    let m = mon();
    let mut detector = DirectedCharmsDetector::new(8, 40, 1000);

    // 1. Absolute physical edge test: cursor at extreme bezel x=1919, y=0
    assert!(detector.update(&m, (1919, 0), 1000).is_empty());
    assert!(
        detector.is_any_armed(),
        "Must arm at absolute physical edge x=1919!"
    );

    // Sliding down strictly on the absolute edge x=1919 (including slight optical overshoot x=1921)
    assert!(detector.update(&m, (1920, 20), 1050).is_empty());
    let fired = detector.update(&m, (1919, 65), 1100);
    assert_eq!(
        fired,
        vec![ChurnZoneId::TopRightDown],
        "Must trigger cleanly on absolute screen edge!"
    );

    // Reset detector
    detector.reset();

    // 2. Wide diagonal sweep rejection test:
    // Arm in corner
    detector.update(&m, (1905, 10), 2000);
    assert!(detector.is_any_armed());

    // Diagonal sweep moving away from bezel deep inwards towards screen center (x=1800, y=150):
    // Must immediately disarm because it exceeds the permitted corridor!
    let diagonal_fired = detector.update(&m, (1800, 150), 2100);
    assert!(
        diagonal_fired.is_empty(),
        "Wide diagonal sweep must not trigger!"
    );
    assert!(
        !detector.is_any_armed(),
        "Moving inward beyond corridor must disarm!"
    );
}

#[test]
fn dismissal_controller_grace_period_and_cursor_protection() {
    let cfg = EdgeStashConfig {
        hide_on_focus_loss: true,
        ..Default::default()
    };
    let fg_ide = WindowHandle(100);
    let app = WindowHandle(200);

    let mut ctrl = DismissalController::default();
    ctrl.on_drawer_opened_at(Some(fg_ide), DockMode::FloatingOverlay, 1000);

    // Stale/intermediate foreground change during first 500ms grace period MUST NOT dismiss
    assert_eq!(
        ctrl.on_foreground_changed_at(fg_ide, &[app], &[], false, 1200, &cfg),
        DismissalAction::None,
        "Grace period protects newly opened drawer from stale focus change"
    );

    // If cursor is over drawer, foreign focus change MUST NOT dismiss
    assert_eq!(
        ctrl.on_foreground_changed_at(fg_ide, &[app], &[], true, 2000, &cfg),
        DismissalAction::None,
        "Cursor over drawer protects it from being hidden"
    );

    // If drawer gained focus, then user clicks outside (cursor not over drawer) after grace period -> dismiss!
    assert_eq!(
        ctrl.on_foreground_changed_at(app, &[app], &[], true, 2100, &cfg),
        DismissalAction::None
    );
    assert_eq!(
        ctrl.on_foreground_changed_at(fg_ide, &[app], &[], false, 2500, &cfg),
        DismissalAction::HideImmediate,
        "Intentional focus switch outside drawer dismisses"
    );
}

#[test]
fn drag_capture_arms_on_modifier_or_edge_dwell_and_commits_on_movesizeend() {
    let monitors = vec![mon()];
    let mut drag = DragCaptureStateMachine::new(4, 350).with_corner_span(32);

    drag.begin_drag(WindowHandle(42));
    // Cursor at right 4px edge (1918, 500) without modifier at t=0 -> not armed yet
    assert_eq!(drag.update_drag(&monitors, (1918, 500), false, 0), None);
    // Still there at t=350ms -> arms!
    assert_eq!(
        drag.update_drag(&monitors, (1918, 500), false, 350),
        Some(DragDropTarget::Edge {
            monitor_id: "MON-1".to_string(),
            edge: ScreenEdge::Right,
        })
    );
    // Moving away disarms
    assert_eq!(drag.update_drag(&monitors, (1700, 500), false, 360), None);

    // Modifier held (e.g. Right-Click while dragging) near left edge arms immediately!
    assert_eq!(
        drag.update_drag(&monitors, (2, 400), true, 400),
        Some(DragDropTarget::Edge {
            monitor_id: "MON-1".to_string(),
            edge: ScreenEdge::Left,
        })
    );
    assert_eq!(
        drag.end_drag(),
        Some((
            WindowHandle(42),
            DragDropTarget::Edge {
                monitor_id: "MON-1".to_string(),
                edge: ScreenEdge::Left,
            }
        ))
    );
}

#[test]
fn dismissal_controller_handles_hysteresis_popup_guard_group_focus_and_dock_pinning() {
    let cfg = EdgeStashConfig {
        mouse_leave_delay_ms: 350,
        focus_restore_delay_ms: 50,
        ..Default::default()
    };

    let fg_ide = WindowHandle(100);
    let notepad = WindowHandle(201);
    let calc = WindowHandle(202);
    let group_rects = vec![
        Rect::new(1520, 0, 1920, 516),
        Rect::new(1520, 516, 1920, 1032),
    ];

    let mut ctrl = DismissalController::default();
    ctrl.on_drawer_opened(Some(fg_ide), DockMode::FloatingOverlay);

    // Cursor initially outside (hotkey launch) -> does NOT auto-close before cursor enters
    assert_eq!(
        ctrl.tick((500, 500), &group_rects, false, 1000, &cfg),
        DismissalAction::None
    );
    assert_eq!(
        ctrl.tick((500, 500), &group_rects, false, 2000, &cfg),
        DismissalAction::None
    );

    // Cursor enters second window in group (Calculator)
    assert_eq!(
        ctrl.tick((1600, 700), &group_rects, false, 2100, &cfg),
        DismissalAction::None
    );

    // Moves within 24px hysteresis margin (1520 - 20 = 1500) -> stays open
    assert_eq!(
        ctrl.tick((1500, 700), &group_rects, false, 2600, &cfg),
        DismissalAction::None
    );

    // Moves outside margin (1490), starts 350ms countdown
    assert_eq!(
        ctrl.tick((1490, 700), &group_rects, false, 3000, &cfg),
        DismissalAction::None
    );

    // Active context menu/popup pauses and resets countdown!
    assert_eq!(
        ctrl.tick((1400, 700), &group_rects, true, 3400, &cfg),
        DismissalAction::None
    );
    assert_eq!(
        ctrl.tick((1400, 700), &group_rects, false, 3500, &cfg),
        DismissalAction::None
    );

    // Re-entering margin cancels countdown
    assert_eq!(
        ctrl.tick((1510, 700), &group_rects, false, 3700, &cfg),
        DismissalAction::None
    );

    // Leaving again and waiting full 350ms triggers hide + focus restore to IDE
    assert_eq!(
        ctrl.tick((1400, 700), &group_rects, false, 4000, &cfg),
        DismissalAction::None
    );
    assert_eq!(
        ctrl.tick((1400, 700), &group_rects, false, 4350, &cfg),
        DismissalAction::HideAndRestoreFocus {
            restore_target: Some(fg_ide),
            delay_ms: 50
        }
    );

    // Clicking between Notepad and Calculator in same group keeps drawer open!
    ctrl.on_drawer_opened(Some(fg_ide), DockMode::FloatingOverlay);
    assert_eq!(
        ctrl.on_foreground_changed(calc, &[notepad, calc], &[], &cfg),
        DismissalAction::None
    );
    // Clicking external IDE hides immediately without stealing focus back
    assert_eq!(
        ctrl.on_foreground_changed(fg_ide, &[notepad, calc], &[], &cfg),
        DismissalAction::HideImmediate
    );

    // NativeWorkAreaDock ignores mouse-leave and focus-loss completely (stays pinned)
    ctrl.on_drawer_opened(Some(fg_ide), DockMode::NativeWorkAreaDock);
    assert_eq!(
        ctrl.tick((1600, 700), &group_rects, false, 5000, &cfg),
        DismissalAction::None
    );
    assert_eq!(
        ctrl.tick((100, 100), &group_rects, false, 6000, &cfg),
        DismissalAction::None
    );
    assert_eq!(
        ctrl.on_foreground_changed(fg_ide, &[notepad, calc], &[], &cfg),
        DismissalAction::None
    );
}

#[test]
fn crash_recovery_manifest_persists_and_restores_orphaned_windows() {
    let dir = std::env::temp_dir().join(format!("edgestash-crash-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let manifest_path = dir.join("active_stashes.json");

    let mut manifest = CrashManifest::default();
    manifest.entries.push(CrashRecoveryEntry {
        hwnd: 777,
        original_ex_style: 0x0004_0100,
        original_rect: Rect::new(200, 150, 900, 750),
        monitor_id: "MON-1".to_string(),
        edge: ScreenEdge::Right,
    });
    manifest.entries.push(CrashRecoveryEntry {
        hwnd: 888, // Dead window no longer exists
        original_ex_style: 0,
        original_rect: Rect::new(0, 0, 400, 400),
        monitor_id: "MON-1".to_string(),
        edge: ScreenEdge::Left,
    });
    manifest.save_atomic(&manifest_path).expect("save manifest");
    assert!(manifest_path.exists());

    let mut restored = Vec::new();
    let count = recover_orphans_with(
        &manifest_path,
        |hwnd| hwnd == WindowHandle(777),
        |hwnd, ex_style, rect| {
            restored.push((hwnd, ex_style, rect));
        },
    )
    .expect("recover orphans");

    assert_eq!(count, 1);
    assert_eq!(
        restored,
        vec![(
            WindowHandle(777),
            0x0004_0100,
            Rect::new(200, 150, 900, 750)
        )]
    );
    let reloaded = CrashManifest::load(&manifest_path).expect("reloaded empty manifest");
    assert!(reloaded.entries.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn shared_corner_routing_corridor_drift_pill_hover_and_shell_fullscreen_filter() {
    let m = mon();
    // Exact top-right corner pixel (1919, 0) overlaps Top and Right strips; candidates include both!
    let candidates = corner_candidate_edges(&m, 8, (1919, 0));
    assert!(candidates.contains(&ScreenEdge::Right));
    assert!(candidates.contains(&ScreenEdge::Top));

    // Armed at top-right edge (1919, 0), drifting inward within 8px corridor (x = 1916) still completes slide
    let mut charms = CharmsStateMachine::new(8, 40, 600);
    assert!(!charms.update(&m, ScreenEdge::Right, (1919, 0), 1000));
    assert!(charms.is_armed());
    assert!(!charms.update(&m, ScreenEdge::Right, (1916, 20), 1080));
    assert!(charms.is_armed());
    assert!(charms.update(&m, ScreenEdge::Right, (1915, 45), 1160));
    assert!(!charms.is_armed());

    // Hover policy per gesture mode
    assert!(allows_pill_hover(GestureMode::EdgeBump));
    assert!(allows_pill_hover(GestureMode::CornerSlide));
    assert!(allows_pill_hover(GestureMode::EdgeHover));
    assert!(!allows_pill_hover(GestureMode::PillClick));
    assert!(!allows_pill_hover(GestureMode::HotkeyOnly));

    assert!(!allows_strip_hover(GestureMode::EdgeBump));
    assert!(!allows_strip_hover(GestureMode::CornerSlide));
    assert!(allows_strip_hover(GestureMode::EdgeHover));

    assert!(allows_pill_click(GestureMode::EdgeBump));
    assert!(allows_pill_click(GestureMode::CornerSlide));
    assert!(allows_pill_click(GestureMode::EdgeHover));
    assert!(allows_pill_click(GestureMode::PillClick));
    assert!(!allows_pill_click(GestureMode::HotkeyOnly));

    // Right edge pill sits strictly at monitor work_rect right edge
    let pill = edge_pill_rect(&m, ScreenEdge::Right);
    assert_eq!(pill.right, m.work_rect.right);
    assert!(pill.width() >= 10 && pill.height() >= 64);

    // Desktop / taskbar / EdgeStash overlays must not disable triggers as "fullscreen"
    assert!(is_non_fullscreen_shell_or_internal_class("Progman"));
    assert!(is_non_fullscreen_shell_or_internal_class("WorkerW"));
    assert!(is_non_fullscreen_shell_or_internal_class("Shell_TrayWnd"));
    assert!(is_non_fullscreen_shell_or_internal_class(
        "EdgeStashD2DOverlay"
    ));
    assert!(is_non_fullscreen_shell_or_internal_class(
        "EdgeStashTriggerStrip"
    ));
    assert!(!is_non_fullscreen_shell_or_internal_class("UnrealWindow"));
}

#[test]
fn app_icons_stack_pill_grows_per_slot_and_hittest_resolves_clicked_icon() {
    let m = mon();
    let p1 = edge_pill_rect_for_style(&m, ScreenEdge::Right, EdgePillStyle::AppIconsStack, 1);
    let p3 = edge_pill_rect_for_style(&m, ScreenEdge::Right, EdgePillStyle::AppIconsStack, 3);
    assert_eq!(p3.right, m.work_rect.right);
    assert!(p3.width() >= 24);
    assert!(p3.height() > p1.height());

    // Clicking slot 0, 1, 2 along vertical stack resolves each exact index
    let local_x = p3.width() / 2;
    let first_y = 4 + 12;
    let mid_y = 4 + 24 + 12;
    let third_y = 4 + 48 + 12;
    assert_eq!(
        hit_test_pill_slot_index(
            p3,
            ScreenEdge::Right,
            EdgePillStyle::AppIconsStack,
            3,
            96,
            (local_x, first_y)
        ),
        Some(0)
    );
    assert_eq!(
        hit_test_pill_slot_index(
            p3,
            ScreenEdge::Right,
            EdgePillStyle::AppIconsStack,
            3,
            96,
            (local_x, mid_y)
        ),
        Some(1)
    );
    assert_eq!(
        hit_test_pill_slot_index(
            p3,
            ScreenEdge::Right,
            EdgePillStyle::AppIconsStack,
            3,
            96,
            (local_x, third_y)
        ),
        Some(2)
    );

    // Horizontal top edge scales width horizontally
    let top3 = edge_pill_rect_for_style(&m, ScreenEdge::Top, EdgePillStyle::AppIconsStack, 3);
    assert_eq!(top3.top, m.work_rect.top);
    assert!(top3.width() > 64);
    assert_eq!(
        hit_test_pill_slot_index(
            top3,
            ScreenEdge::Top,
            EdgePillStyle::AppIconsStack,
            3,
            96,
            (4 + 24 + 12, top3.height() / 2)
        ),
        Some(1)
    );
}

#[test]
fn dim_scrim_fade_interpolates_smoothly_reverses_midfade_and_handles_zero_duration() {
    let mut fade = DimScrimFade::default();
    fade.set_target(0.40, 200, 1000);
    let (a0, anim0) = fade.advance(1000);
    assert!((a0 - 0.0).abs() < 0.001 && anim0);

    let (a100, anim100) = fade.advance(1100);
    assert!(a100 > 0.20 && a100 < 0.40 && anim100);

    // Mid-fade reversal at 1100ms back toward 0.0 keeps current alpha without jumping to 0.40
    fade.set_target(0.0, 200, 1100);
    let (a_rev_start, anim_rev) = fade.advance(1100);
    assert!((a_rev_start - a100).abs() < 0.001 && anim_rev);
    let (a_rev_end, anim_done) = fade.advance(1300);
    assert!((a_rev_end - 0.0).abs() < 0.001 && !anim_done);

    // Zero duration jumps immediately
    fade.set_target(0.35, 0, 1500);
    let (a_inst, anim_inst) = fade.advance(1500);
    assert!((a_inst - 0.35).abs() < 0.001 && !anim_inst);
}

#[test]
fn area_aware_scroll_controller_supports_disabled_initial_always_and_strict_modes() {
    // 1. Disabled -> never intercepts thin edge strip
    let mut ctrl = AreaAwareScrollController::default();
    ctrl.on_strip_enter(false);
    assert_eq!(
        ctrl.on_strip_wheel(
            false,
            AreaAwareScrollMode::InitialTriggerSession,
            false,
            false,
            true,
            true
        ),
        StripScrollDecision::PassToWindow
    );

    // 2. InitialTriggerSession (Default): cycles while on strip (even before open -> opens & cycles),
    // disarms empty strip once moving into drawer, re-arms after drawer closes
    let mut initial = AreaAwareScrollController::default();
    initial.on_strip_enter(false);
    assert_eq!(
        initial.on_strip_wheel(
            true,
            AreaAwareScrollMode::InitialTriggerSession,
            false,
            false,
            true,
            true
        ),
        StripScrollDecision::CycleAndOpenIfClosed
    );
    initial.on_drawer_opened(true, true);
    assert_eq!(
        initial.on_strip_wheel(
            true,
            AreaAwareScrollMode::InitialTriggerSession,
            false,
            true,
            true,
            true
        ),
        StripScrollDecision::CycleWindow
    );
    // Leaves thin line into drawer window -> disarms empty strip
    initial.on_strip_leave(true);
    assert!(!initial.should_strip_capture_pointer(
        true,
        AreaAwareScrollMode::InitialTriggerSession,
        false,
        true,
        true,
    ));
    assert_eq!(
        initial.on_strip_wheel(
            true,
            AreaAwareScrollMode::InitialTriggerSession,
            false,
            true,
            true,
            true
        ),
        StripScrollDecision::PassToWindow
    );
    // Closing & re-opening resets cleanly
    initial.on_drawer_closed();
    initial.on_strip_enter(false);
    assert_eq!(
        initial.on_strip_wheel(
            true,
            AreaAwareScrollMode::InitialTriggerSession,
            false,
            false,
            true,
            true
        ),
        StripScrollDecision::CycleAndOpenIfClosed
    );

    // 3. AlwaysOnTriggerLine: returning from drawer back onto thin strip keeps cycling
    let mut always = AreaAwareScrollController::default();
    always.on_strip_enter(false);
    always.on_drawer_opened(true, true);
    always.on_strip_leave(true);
    always.on_strip_enter(true);
    assert!(always.should_strip_capture_pointer(
        true,
        AreaAwareScrollMode::AlwaysOnTriggerLine,
        false,
        true,
        true,
    ));
    assert_eq!(
        always.on_strip_wheel(
            true,
            AreaAwareScrollMode::AlwaysOnTriggerLine,
            false,
            true,
            true,
            true
        ),
        StripScrollDecision::CycleWindow
    );

    // 4. OpenSessionStrict (New Default): never captures when closed or hotkey-opened;
    // works once opened from edge until leaving thin strip
    let mut strict = AreaAwareScrollController::default();
    strict.on_strip_enter(false);
    assert!(!strict.should_strip_capture_pointer(
        true,
        AreaAwareScrollMode::OpenSessionStrict,
        false,
        false,
        true,
    ));
    assert_eq!(
        strict.on_strip_wheel(
            true,
            AreaAwareScrollMode::OpenSessionStrict,
            false,
            false,
            true,
            true
        ),
        StripScrollDecision::PassToWindow
    );
    strict.on_drawer_opened(true, true);
    assert!(strict.is_shade_visible(
        true,
        AreaAwareScrollMode::OpenSessionStrict,
        false,
        true,
        true,
        true,
    ));
    assert_eq!(
        strict.on_strip_wheel(
            true,
            AreaAwareScrollMode::OpenSessionStrict,
            false,
            true,
            true,
            true
        ),
        StripScrollDecision::CycleWindow
    );
    strict.on_strip_leave(true);
    assert!(!strict.is_shade_visible(
        true,
        AreaAwareScrollMode::OpenSessionStrict,
        false,
        true,
        true,
        false,
    ));
    assert!(!strict.should_strip_capture_pointer(
        true,
        AreaAwareScrollMode::OpenSessionStrict,
        false,
        true,
        true,
    ));
    assert_eq!(
        strict.on_strip_wheel(
            true,
            AreaAwareScrollMode::OpenSessionStrict,
            false,
            true,
            true,
            true
        ),
        StripScrollDecision::PassToWindow
    );

    let mut hotkey_strict = AreaAwareScrollController::default();
    hotkey_strict.on_drawer_opened(false, false);
    assert!(!hotkey_strict.should_strip_capture_pointer(
        true,
        AreaAwareScrollMode::OpenSessionStrict,
        false,
        true,
        true,
    ));
}

#[test]
fn drag_drop_zone_detection_corners_and_edges() {
    let monitors = vec![mon()];

    // 1. Dedicated corner drop zones (< 96px in both X and Y)
    assert_eq!(
        detect_drag_drop_zone(&monitors, 80, 96, (20, 20)),
        Some(DragDropTarget::Corner {
            monitor_id: "MON-1".to_string(),
            zone: ChurnZoneId::TopLeftDown,
        })
    );
    assert_eq!(
        detect_drag_drop_zone(&monitors, 80, 96, (1900, 20)),
        Some(DragDropTarget::Corner {
            monitor_id: "MON-1".to_string(),
            zone: ChurnZoneId::TopRightDown,
        })
    );
    assert_eq!(
        detect_drag_drop_zone(&monitors, 80, 96, (20, 1060)),
        Some(DragDropTarget::Corner {
            monitor_id: "MON-1".to_string(),
            zone: ChurnZoneId::BottomLeftUp,
        })
    );
    assert_eq!(
        detect_drag_drop_zone(&monitors, 80, 96, (1900, 1060)),
        Some(DragDropTarget::Corner {
            monitor_id: "MON-1".to_string(),
            zone: ChurnZoneId::BottomRightUp,
        })
    );

    // 2. Center edge drop zones (within 80px of edge, not in 96px corner)
    assert_eq!(
        detect_drag_drop_zone(&monitors, 80, 96, (30, 500)),
        Some(DragDropTarget::Edge {
            monitor_id: "MON-1".to_string(),
            edge: ScreenEdge::Left,
        })
    );
    assert_eq!(
        detect_drag_drop_zone(&monitors, 80, 96, (1900, 500)),
        Some(DragDropTarget::Edge {
            monitor_id: "MON-1".to_string(),
            edge: ScreenEdge::Right,
        })
    );
    assert_eq!(
        detect_drag_drop_zone(&monitors, 80, 96, (960, 30)),
        Some(DragDropTarget::Edge {
            monitor_id: "MON-1".to_string(),
            edge: ScreenEdge::Top,
        })
    );
    assert_eq!(
        detect_drag_drop_zone(&monitors, 80, 96, (960, 1050)),
        Some(DragDropTarget::Edge {
            monitor_id: "MON-1".to_string(),
            edge: ScreenEdge::Bottom,
        })
    );

    // 3. Center of screen (> 80px from all perimeters) -> None
    assert_eq!(detect_drag_drop_zone(&monitors, 80, 96, (960, 540)), None);

    // 4. Preview rect calculation for edge vs corner
    let corner_target = DragDropTarget::Corner {
        monitor_id: "MON-1".to_string(),
        zone: ChurnZoneId::TopRightDown,
    };
    let corner_rect = compute_drop_preview_rect(&monitors[0], &corner_target);
    assert_eq!(corner_rect, Rect::new(1920 - 96, 0, 1920, 96));

    let edge_target = DragDropTarget::Edge {
        monitor_id: "MON-1".to_string(),
        edge: ScreenEdge::Right,
    };
    let edge_rect = compute_drop_preview_rect(&monitors[0], &edge_target);
    assert_eq!(edge_rect.left, 1920 - 36);
    assert_eq!(edge_rect.right, 1920 + 2);
}

#[test]
fn scroll_trigger_line_only_when_open_setting_suppresses_closed_wheel_and_shade() {
    let mut ctrl = AreaAwareScrollController::default();
    ctrl.on_strip_enter(false);

    // When scroll_trigger_line_only_when_open is true and drawer is closed:
    // Should NOT capture pointer, shade is NOT visible, wheel passes through to window
    assert!(!ctrl.should_strip_capture_pointer(
        true,
        AreaAwareScrollMode::AlwaysOnTriggerLine,
        true,  // scroll_trigger_line_only_when_open
        false, // is_open = false
        true,
    ));
    assert!(!ctrl.is_shade_visible(
        true,
        AreaAwareScrollMode::AlwaysOnTriggerLine,
        true,  // scroll_trigger_line_only_when_open
        false, // is_open = false
        true,
        true, // cursor_on_line
    ));
    assert_eq!(
        ctrl.on_strip_wheel(
            true,
            AreaAwareScrollMode::AlwaysOnTriggerLine,
            true,  // scroll_trigger_line_only_when_open
            false, // is_open = false
            true,  // cursor_on_strip
            true,
        ),
        StripScrollDecision::PassToWindow
    );

    // Once drawer opens (is_open = true):
    // Captures pointer, shade becomes visible, wheel cycles windows!
    ctrl.on_drawer_opened(true, true);
    assert!(ctrl.should_strip_capture_pointer(
        true,
        AreaAwareScrollMode::AlwaysOnTriggerLine,
        true, // scroll_trigger_line_only_when_open
        true, // is_open = true
        true,
    ));
    assert!(ctrl.is_shade_visible(
        true,
        AreaAwareScrollMode::AlwaysOnTriggerLine,
        true, // scroll_trigger_line_only_when_open
        true, // is_open = true
        true,
        true,
    ));
    assert_eq!(
        ctrl.on_strip_wheel(
            true,
            AreaAwareScrollMode::AlwaysOnTriggerLine,
            true, // scroll_trigger_line_only_when_open
            true, // is_open = true
            true,
            true,
        ),
        StripScrollDecision::CycleWindow
    );
}

#[test]
fn non_blocking_strip_4px_shade_auto_hide_pill_directed_churn_and_scrim_zorder() {
    let m = mon();

    // Bug #4: closed CornerSlide or empty edge must NEVER block normal right-edge scrolling/clicks
    assert!(is_strip_input_transparent(
        GestureMode::EdgeBump,
        true,
        AreaAwareScrollMode::OpenSessionStrict,
        false,
        true,
    ));
    assert!(is_strip_input_transparent(
        GestureMode::CornerSlide,
        true,
        AreaAwareScrollMode::OpenSessionStrict,
        false,
        true,
    ));
    assert!(is_strip_input_transparent(
        GestureMode::CornerSlide,
        false,
        AreaAwareScrollMode::AlwaysOnTriggerLine,
        false,
        true,
    ));
    assert!(is_strip_input_transparent(
        GestureMode::CornerSlide,
        true,
        AreaAwareScrollMode::AlwaysOnTriggerLine,
        false,
        false,
    ));

    // Feature #5: hairline 4px shade bar sits on work_rect bezel matching edge width/height
    assert_eq!(
        area_scroll_shade_rect(&m, ScreenEdge::Right),
        Rect::new(1916, 0, 1920, 1032)
    );
    assert_eq!(
        area_scroll_shade_rect(&m, ScreenEdge::Bottom),
        Rect::new(0, 1028, 1920, 1032)
    );

    // Feature #6: Edge Indicator Pill Auto-Hide only shows when drawer open OR close animation running
    assert!(!should_pill_be_visible(
        EdgePillStyle::AppIconsStack,
        true,
        2,
        false,
        false,
        false
    ));
    assert!(should_pill_be_visible(
        EdgePillStyle::AppIconsStack,
        true,
        2,
        true,
        false,
        false
    ));
    assert!(should_pill_be_visible(
        EdgePillStyle::AppIconsStack,
        true,
        0,
        false,
        true,
        false
    ));
    assert!(should_pill_be_visible(
        EdgePillStyle::AppIconsStack,
        false,
        2,
        false,
        false,
        false
    ));
    assert!(!should_pill_be_visible(
        EdgePillStyle::Hidden,
        false,
        2,
        true,
        false,
        false
    ));
    // Feature: Window Attention surfaces pill even when Auto-Hide is active and drawer is closed
    assert!(should_pill_be_visible(
        EdgePillStyle::AppIconsStack,
        true,
        2,
        false,
        false,
        true
    ));

    // Bug #3 + Feature #7: DirectedCharmsDetector distinguishes all 8 corner-slide directions cleanly
    let mut detector = DirectedCharmsDetector::new(8, 40, 600);
    // Bottom-right corner sliding UP -> fires BottomRightUp only (not BottomRightLeft or TopRightDown)
    assert!(detector.update(&m, (1916, 1077), 1000).is_empty());
    let fired_up = detector.update(&m, (1915, 1025), 1120);
    assert_eq!(fired_up, vec![ChurnZoneId::BottomRightUp]);

    // Problem 1: Top-right corner sliding DOWN with up to 36px inward drift (x = 1890 on 1920-wide monitor)
    // must successfully trigger TopRightDown without aborting!
    assert!(detector.update(&m, (1800, 500), 1150).is_empty());
    assert!(detector.update(&m, (1918, 4), 1200).is_empty());
    assert!(detector.is_any_armed());
    let fired_down = detector.update(&m, (1890, 50), 1260);
    assert_eq!(fired_down, vec![ChurnZoneId::TopRightDown]);

    // Leaving bezel resets; bottom-right corner sliding LEFT -> fires BottomRightLeft only
    assert!(detector.update(&m, (1800, 900), 1300).is_empty());
    assert!(detector.update(&m, (1916, 1077), 1350).is_empty());
    let fired_left = detector.update(&m, (1860, 1075), 1400);
    assert_eq!(fired_left, vec![ChurnZoneId::BottomRightLeft]);

    // Bug #1: dim scrim must insert directly AFTER lowest open floating drawer (never HWND_TOPMOST above drawers)
    assert_eq!(compute_dim_scrim_insert_after(&[]), None);
    assert_eq!(
        compute_dim_scrim_insert_after(&[WindowHandle(101), WindowHandle(202)]),
        Some(WindowHandle(202))
    );
}

#[test]
fn dismissal_controller_ignores_pin_and_active_drag_until_released() {
    let cfg = EdgeStashConfig {
        mouse_leave_delay_ms: 200,
        ..Default::default()
    };
    let fg = WindowHandle(1);
    let stashed = WindowHandle(2);
    let rects = vec![Rect::new(1520, 0, 1920, 1032)];

    let mut ctrl = DismissalController::default();
    ctrl.on_drawer_opened(Some(fg), DockMode::FloatingPinned);
    assert_eq!(
        ctrl.tick((1600, 500), &rects, false, 1000, &cfg),
        DismissalAction::None
    );
    assert_eq!(
        ctrl.tick((200, 500), &rects, false, 5000, &cfg),
        DismissalAction::None
    );
    assert_eq!(
        ctrl.on_foreground_changed(fg, &[stashed], &[], &cfg),
        DismissalAction::None
    );

    // Active drag protects FloatingOverlay from hiding while dragging out/in
    ctrl.on_drawer_opened(Some(fg), DockMode::FloatingOverlay);
    ctrl.set_drag_in_progress(true);
    assert_eq!(
        ctrl.tick((1600, 500), &rects, false, 100, &cfg),
        DismissalAction::None
    );
    assert_eq!(
        ctrl.tick((1000, 500), &rects, false, 600, &cfg),
        DismissalAction::None
    );
    ctrl.set_drag_in_progress(false);
    assert_eq!(
        ctrl.tick((1000, 500), &rects, false, 650, &cfg),
        DismissalAction::None
    );
    assert_eq!(
        ctrl.tick((1000, 500), &rects, false, 850, &cfg),
        DismissalAction::HideAndRestoreFocus {
            restore_target: Some(fg),
            delay_ms: cfg.focus_restore_delay_ms,
        }
    );
}

#[test]
fn directed_charms_angled_corridor_and_ergonomic_bezel_pushing() {
    let m = mon();

    // 1. Ergonomic outward bezel pushing:
    // User moves cursor to top-right corner (1920, 2), then pulls diagonally downwards
    // while physically pushing rightwards into the screen boundary (cursor coordinates 1920..1922).
    let mut det = DirectedCharmsDetector::new(8, 40, 600);
    det.sync_params_dpi(8, 40, 600, 45, 96);

    // Enter corner at the absolute edge (1920, 2)
    assert!(det.update(&m, (1920, 2), 100).is_empty());
    assert!(det.is_any_armed());

    // Pull down while pushing against right bezel (1920..1922)
    assert!(det.update(&m, (1921, 15), 150).is_empty());
    assert!(det.update(&m, (1920, 30), 200).is_empty());
    let fired = det.update(&m, (1920, 45), 250);
    assert_eq!(fired, vec![ChurnZoneId::TopRightDown]);

    // 2. Inward diagonal drift with default 45° angle:
    // With 45° angle, tan(45°) = 1.0. As cursor slides 40px down, inward corridor width
    // expands from base 36px to 36 + 40 = 76px (down to x = 1920 - 76 = 1844).
    det.reset();
    assert!(det.update(&m, (1918, 0), 1000).is_empty());
    assert!(det.is_any_armed());

    // Diagonal inward slide to (1880, 50) (inward distance = 40px <= 86px allowed, along Y = 50px):
    let fired_inward = det.update(&m, (1880, 50), 1100);
    assert_eq!(fired_inward, vec![ChurnZoneId::TopRightDown]);

    // 3. Steep inward drift exceeding 45° angle is rejected:
    det.reset();
    assert!(det.update(&m, (1918, 0), 2000).is_empty());
    // Moving deep into screen center (x = 1800, y = 150, inward distance = 120px > 68px allowed)
    assert!(det.update(&m, (1800, 150), 2100).is_empty());
    assert!(
        !det.is_any_armed(),
        "Steep inward drift beyond 45° must disarm to Idle"
    );

    // 4. Configurable angle: with 65° allowed angle, tan(65°) ≈ 2.14, inward expansion at 40px is ~86px,
    // so downward slide to (1840, 90) (inward drift = 78px, along Y = 90px) IS allowed!
    det.reset();
    det.sync_params_dpi(8, 40, 600, 65, 96);
    assert!(det.update(&m, (1918, 0), 3000).is_empty());
    let fired_wide = det.update(&m, (1840, 90), 3100);
    assert_eq!(fired_wide, vec![ChurnZoneId::TopRightDown]);

    // 5. Narrow angle: with 20° angle, tan(20°) ≈ 0.364, inward expansion at 40px is ~15px,
    // total corridor width = 28 + 15 = 43px, so (1865, 50) (inward 55px) is REJECTED!
    det.reset();
    det.sync_params_dpi(8, 40, 600, 20, 96);
    assert!(det.update(&m, (1918, 0), 4000).is_empty());
    assert!(det.update(&m, (1865, 50), 4100).is_empty());
    assert!(
        !det.is_any_armed(),
        "Drift exceeding narrow 20° corridor must disarm to Idle"
    );
}

#[test]
fn compute_churn_preview_rect_positions_on_screen_edges() {
    let m = mon();

    // TopRightDown: anchored at top-right corner of monitor (1920, 0), expands down and left
    let r_trd = compute_churn_preview_rect(&m, ChurnZoneId::TopRightDown, 24, 60, 45);
    assert_eq!(r_trd.right, m.rect.right);
    assert_eq!(r_trd.top, m.rect.top);
    assert!(r_trd.width() >= 100);
    assert!(r_trd.height() >= 100);

    // BottomRightUp: anchored at bottom-right corner of monitor (1920, 1080), expands up and left
    let r_bru = compute_churn_preview_rect(&m, ChurnZoneId::BottomRightUp, 24, 60, 45);
    assert_eq!(r_bru.right, m.rect.right);
    assert_eq!(r_bru.bottom, m.rect.bottom);

    // TopLeftDown: anchored at top-left corner of monitor (0, 0), expands down and right
    let r_tld = compute_churn_preview_rect(&m, ChurnZoneId::TopLeftDown, 24, 60, 45);
    assert_eq!(r_tld.left, m.rect.left);
    assert_eq!(r_tld.top, m.rect.top);

    // TopRightLeft: horizontal churn on Top edge, right corner, slides left
    let r_trl = compute_churn_preview_rect(&m, ChurnZoneId::TopRightLeft, 24, 60, 45);
    assert_eq!(r_trl.right, m.rect.right);
    assert_eq!(r_trl.top, m.rect.top);
}

#[test]
fn drag_drop_tri_zone_band_detection_and_settings_tests() {
    let m = mon();
    let monitors = vec![m.clone()];

    // 1. Edge band rect covering the tri-zone preview
    let right_band = compute_edge_band_rect(&m, ScreenEdge::Right, 80, 96);
    assert_eq!(right_band, Rect::new(1920 - 96, 0, 1920, 1080));

    let left_band = compute_edge_band_rect(&m, ScreenEdge::Left, 80, 96);
    assert_eq!(left_band, Rect::new(0, 0, 96, 1080));

    let top_band = compute_edge_band_rect(&m, ScreenEdge::Top, 80, 96);
    assert_eq!(top_band, Rect::new(0, 0, 1920, 96));

    let bottom_band = compute_edge_band_rect(&m, ScreenEdge::Bottom, 80, 96);
    assert_eq!(bottom_band, Rect::new(0, 1080 - 96, 1920, 1080));

    // 2. Proximity detection near edges
    let near_right = detect_near_edge(&monitors, 100, (1850, 500));
    assert_eq!(near_right.map(|(_, e)| e), Some(ScreenEdge::Right));

    let near_top = detect_near_edge(&monitors, 100, (500, 50));
    assert_eq!(near_top.map(|(_, e)| e), Some(ScreenEdge::Top));

    let far_center = detect_near_edge(&monitors, 100, (960, 540));
    assert_eq!(far_center, None);

    // 3. DragCaptureStateMachine dwell and immediate hover feedback
    let mut sm = DragCaptureStateMachine::new(80, 200).with_corner_span(96);
    assert!(!sm.is_dragging());
    assert_eq!(sm.hovering_target(), None);
    assert_eq!(sm.armed_target(), None);

    let dummy_hwnd = WindowHandle(0x1234);
    sm.begin_drag(dummy_hwnd);
    assert!(sm.is_dragging());

    // In center-right edge zone at t=1000
    let armed_1 = sm.update_drag(&monitors, (1900, 500), false, 1000);
    assert_eq!(armed_1, None); // Dwell not satisfied yet (200ms)
    assert!(sm.hovering_target().is_some()); // Immediate hover target present for visual highlight
    assert_eq!(
        sm.hovering_target(),
        Some(&DragDropTarget::Edge {
            monitor_id: "MON-1".to_string(),
            edge: ScreenEdge::Right,
        })
    );

    // At t=1100 (100ms later), still hovering, not armed yet
    let armed_2 = sm.update_drag(&monitors, (1900, 500), false, 1100);
    assert_eq!(armed_2, None);

    // At t=1200 (200ms reached), armed!
    let armed_3 = sm.update_drag(&monitors, (1900, 500), false, 1200);
    assert_eq!(
        armed_3,
        Some(DragDropTarget::Edge {
            monitor_id: "MON-1".to_string(),
            edge: ScreenEdge::Right,
        })
    );
    assert_eq!(sm.armed_target(), armed_3.as_ref());

    // Dropping window
    let dropped = sm.end_drag();
    assert_eq!(
        dropped,
        Some((
            dummy_hwnd,
            DragDropTarget::Edge {
                monitor_id: "MON-1".to_string(),
                edge: ScreenEdge::Right,
            }
        ))
    );
    assert!(!sm.is_dragging());

    // 4. Config defaults and clamping
    let mut cfg = EdgeStashConfig::default();
    assert!(cfg.enable_drag_drop_zones);
    assert_eq!(cfg.drag_drop_zone_px, 80);
    assert_eq!(cfg.drag_corner_zone_px, 96);
    assert_eq!(cfg.drag_drop_dwell_ms, 200);

    cfg.drag_drop_zone_px = 10;
    cfg.drag_corner_zone_px = 500;
    cfg.drag_drop_dwell_ms = 5000;
    cfg.validate_and_clamp();
    assert_eq!(cfg.drag_drop_zone_px, 30); // clamped to min 30
    assert_eq!(cfg.drag_corner_zone_px, 240); // clamped to max 240
    assert_eq!(cfg.drag_drop_dwell_ms, 1000); // clamped to max 1000
}

#[test]
fn drag_drop_top_default_disabled_hotkey_toggle_and_inset_snap_guard() {
    let m = mon();
    let monitors = vec![m.clone()];

    // 1. Config defaults: top is disabled by default to avoid titlebar & Win11 snap conflicts!
    let mut cfg = EdgeStashConfig::default();
    assert!(cfg.drag_drop_left);
    assert!(cfg.drag_drop_right);
    assert!(
        !cfg.drag_drop_top,
        "Top edge drop zones must be disabled by default"
    );
    assert!(cfg.drag_drop_bottom);
    assert!(!cfg.drag_drop_require_modifier);
    assert_eq!(cfg.drag_drop_modifier_key, DragDropModifierKey::Shift);
    assert_eq!(cfg.drag_drop_inset_px, 48);

    assert!(cfg.is_drag_drop_edge_enabled(ScreenEdge::Left));
    assert!(cfg.is_drag_drop_edge_enabled(ScreenEdge::Right));
    assert!(!cfg.is_drag_drop_edge_enabled(ScreenEdge::Top));
    assert!(cfg.is_drag_drop_edge_enabled(ScreenEdge::Bottom));

    // 2. Top edge disabled ignores top edge dragging
    let top_cursor = (960, 30);
    assert_eq!(
        detect_drag_drop_zone_ext(
            &monitors,
            80,
            96,
            0,
            |e| cfg.is_drag_drop_edge_enabled(e),
            top_cursor
        ),
        None,
        "Disabled top edge must never trigger drop stashing"
    );

    // If user explicitly enables top edge, it triggers
    cfg.drag_drop_top = true;
    assert_eq!(
        detect_drag_drop_zone_ext(
            &monitors,
            80,
            96,
            0,
            |e| cfg.is_drag_drop_edge_enabled(e),
            top_cursor
        ),
        Some(DragDropTarget::Edge {
            monitor_id: "MON-1".to_string(),
            edge: ScreenEdge::Top,
        })
    );

    // 3. Inset pushes drop zones away from physical bezels to avoid Windows 11 Snap Layouts
    let inset = 48;
    let right_band_inset = compute_edge_band_rect_inset(&m, ScreenEdge::Right, 80, 96, inset);
    assert_eq!(right_band_inset.right, 1920 - inset);
    assert_eq!(right_band_inset.left, 1920 - 96 - inset);
    assert_eq!(right_band_inset.top, inset);
    assert_eq!(right_band_inset.bottom, 1080 - inset);

    // Cursor inside the inset zone (1872 - 50 = 1822, Y=500)
    let inside_inset_cursor = (1850, 500);
    assert_eq!(
        detect_drag_drop_zone_ext(
            &monitors,
            80,
            96,
            inset,
            |e| cfg.is_drag_drop_edge_enabled(e),
            inside_inset_cursor
        ),
        Some(DragDropTarget::Edge {
            monitor_id: "MON-1".to_string(),
            edge: ScreenEdge::Right,
        })
    );

    // Cursor touching physical edge (1918) does NOT activate the inset zone (guarding Win11 snap)
    let physical_edge_cursor = (1918, 500);
    assert_eq!(
        detect_drag_drop_zone_ext(
            &monitors,
            80,
            96,
            inset,
            |e| cfg.is_drag_drop_edge_enabled(e),
            physical_edge_cursor
        ),
        None,
        "Cursor touching physical bezel must not activate inset drop zone (allowing native Win11 snap)"
    );

    // 4. Require modifier (Shift + Drag) toggle
    let mut sm = DragCaptureStateMachine::new(80, 200)
        .with_corner_span(96)
        .with_inset(48)
        .with_require_modifier(true);

    sm.begin_drag(WindowHandle(0x5678));

    // Without modifier: returns None even inside zone
    let armed_no_mod = sm.update_drag(&monitors, (1850, 500), false, 1000);
    assert_eq!(armed_no_mod, None);
    assert_eq!(sm.hovering_target(), None);

    // With modifier held: detects and arms
    let armed_with_mod = sm.update_drag(&monitors, (1850, 500), true, 1000);
    assert_eq!(
        armed_with_mod,
        Some(DragDropTarget::Edge {
            monitor_id: "MON-1".to_string(),
            edge: ScreenEdge::Right,
        })
    );

    // 5. detect_near_edge_ext respects disabled edges and inset
    cfg.drag_drop_top = true;
    let near_top = detect_near_edge_ext(
        &monitors,
        100,
        48,
        |e| cfg.is_drag_drop_edge_enabled(e),
        (960, 50),
    );
    assert_eq!(near_top.map(|(_, e)| e), Some(ScreenEdge::Top));

    cfg.drag_drop_top = false;
    let near_top_disabled = detect_near_edge_ext(
        &monitors,
        100,
        48,
        |e| cfg.is_drag_drop_edge_enabled(e),
        (960, 50),
    );
    assert_eq!(
        near_top_disabled, None,
        "Disabled top edge must never be picked by proximity detection"
    );
}
