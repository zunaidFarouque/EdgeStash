use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

use edgestash_daemon::platform::{
    AppBarApi, Clock, FakeClock, MonitorInfo, PillOverlayEvent, UnstashTargetState,
    VirtualDesktopApi, VisualOverlayApi, WindowAccessError, WindowHandle, WindowSnapshot,
    WindowSystem, WindowTransition,
};
use edgestash_daemon::state::EdgeStashEngine;
use edgestash_ipc::config::{ChurnZoneAction, EdgeStashConfig};
use edgestash_ipc::protocol::IpcErrorCode;
use edgestash_ipc::types::{
    AnimationStyle, ChurnZoneId, DockMode, Rect, ScreenEdge, VirtualDesktopMode,
};
use windows::core::GUID;

#[derive(Default)]
struct MockPlatformState {
    monitors: Vec<MonitorInfo>,
    windows: HashMap<WindowHandle, MockWindow>,
    foreground: Option<WindowHandle>,
    cursor: (i32, i32),
    fullscreen_active: bool,
    current_desktop: GUID,
    window_desktops: HashMap<WindowHandle, GUID>,
    appbars: Vec<(String, ScreenEdge, i32, bool)>,
    overlay_events: Vec<PillOverlayEvent>,
    animated_batches: Vec<(Vec<WindowTransition>, AnimationStyle, bool)>,
    topmost_calls: Vec<(WindowHandle, bool)>,
    restored_focus_calls: Vec<WindowHandle>,
    closed_window_calls: Vec<WindowHandle>,
    ensure_restored_calls: Vec<WindowHandle>,
    refresh_rendering_calls: Vec<WindowHandle>,
    hide_from_taskbar_calls: Vec<WindowHandle>,
    simulate_shrunk_work_area_on_dock: bool,
}

#[derive(Clone)]
struct MockWindow {
    title: String,
    process_name: String,
    rect: Rect,
    ex_style: isize,
    cloaked: bool,
    visible: bool,
    is_iconic: bool,
    is_zoomed: bool,
    has_popup: bool,
    protected: bool,
}

#[derive(Clone)]
struct MockPlatform {
    inner: Rc<RefCell<MockPlatformState>>,
}

impl MockPlatform {
    fn new() -> Self {
        let primary = MonitorInfo {
            id: "MON-1".to_string(),
            rect: Rect::new(0, 0, 1920, 1080),
            work_rect: Rect::new(0, 0, 1920, 1032),
            dpi: 96,
            is_primary: true,
        };
        let st = MockPlatformState {
            monitors: vec![primary],
            current_desktop: GUID::from_u128(0xAAAA_BBBB_CCCC_DDDD_1111_2222_3333_4444),
            ..Default::default()
        };
        Self {
            inner: Rc::new(RefCell::new(st)),
        }
    }

    fn add_window(&self, id: isize, title: &str, rect: Rect) -> WindowHandle {
        self.add_window_with_process(id, title, &format!("{}.exe", title.to_lowercase()), rect)
    }

    fn add_window_with_process(
        &self,
        id: isize,
        title: &str,
        process_name: &str,
        rect: Rect,
    ) -> WindowHandle {
        let hwnd = WindowHandle(id);
        let mut st = self.inner.borrow_mut();
        let desktop = st.current_desktop;
        st.windows.insert(
            hwnd,
            MockWindow {
                title: title.to_string(),
                process_name: process_name.to_string(),
                rect,
                ex_style: 0x0004_0100,
                cloaked: false,
                visible: true,
                is_iconic: false,
                is_zoomed: false,
                has_popup: false,
                protected: false,
            },
        );
        st.window_desktops.insert(hwnd, desktop);
        hwnd
    }
}

impl WindowSystem for MockPlatform {
    fn get_foreground_window(&self) -> Option<WindowHandle> {
        self.inner.borrow().foreground
    }
    fn get_cursor_pos(&self) -> Option<(i32, i32)> {
        Some(self.inner.borrow().cursor)
    }
    fn is_window(&self, hwnd: WindowHandle) -> bool {
        self.inner.borrow().windows.contains_key(&hwnd)
    }
    fn is_window_visible(&self, hwnd: WindowHandle) -> bool {
        self.inner
            .borrow()
            .windows
            .get(&hwnd)
            .is_some_and(|w| w.visible)
    }
    fn snapshot_window(&self, hwnd: WindowHandle) -> Result<WindowSnapshot, WindowAccessError> {
        let st = self.inner.borrow();
        let w = st
            .windows
            .get(&hwnd)
            .ok_or(WindowAccessError::InvalidWindow)?;
        if w.protected {
            return Err(WindowAccessError::ProtectedShellWindow);
        }
        Ok(WindowSnapshot {
            hwnd,
            title: w.title.clone(),
            process_name: w.process_name.clone(),
            class_name: "TestClass".to_string(),
            pid: 1000 + (hwnd.0 as u32),
            window_rect: w.rect,
            extended_frame_rect: w.rect,
            ex_style: w.ex_style,
            has_caption: true,
        })
    }
    fn get_window_rect(&self, hwnd: WindowHandle) -> Option<Rect> {
        self.inner.borrow().windows.get(&hwnd).map(|w| w.rect)
    }
    fn has_active_child_popup(&self, hwnd: WindowHandle) -> bool {
        self.inner
            .borrow()
            .windows
            .get(&hwnd)
            .is_some_and(|w| w.has_popup)
    }
    fn force_restore_foreground_window(&self, target: WindowHandle) {
        let mut st = self.inner.borrow_mut();
        st.foreground = Some(target);
        st.restored_focus_calls.push(target);
    }
    fn set_foreground_window(&self, target: WindowHandle) {
        self.inner.borrow_mut().foreground = Some(target);
    }
    fn prepare_window_for_stash(
        &self,
        hwnd: WindowHandle,
        _ex_style: isize,
    ) -> Result<(), WindowAccessError> {
        let mut st = self.inner.borrow_mut();
        let w = st
            .windows
            .get_mut(&hwnd)
            .ok_or(WindowAccessError::InvalidWindow)?;
        w.ex_style |= 0x80; // WS_EX_TOOLWINDOW
        Ok(())
    }
    fn hide_from_taskbar(&self, hwnd: WindowHandle) {
        self.inner.borrow_mut().hide_from_taskbar_calls.push(hwnd);
    }
    fn restore_window_from_stash(
        &self,
        hwnd: WindowHandle,
        original_ex_style: isize,
        original_rect: Rect,
    ) {
        self.restore_window_from_stash_with_state(
            hwnd,
            original_ex_style,
            original_rect,
            UnstashTargetState::Normal,
        );
    }
    fn restore_window_from_stash_with_state(
        &self,
        hwnd: WindowHandle,
        original_ex_style: isize,
        original_rect: Rect,
        target_state: UnstashTargetState,
    ) {
        let mut st = self.inner.borrow_mut();
        if let Some(w) = st.windows.get_mut(&hwnd) {
            w.ex_style = original_ex_style;
            w.cloaked = false;
            match target_state {
                UnstashTargetState::Normal => {
                    w.rect = original_rect;
                    w.is_iconic = false;
                    w.is_zoomed = false;
                }
                UnstashTargetState::Minimize => {
                    w.is_iconic = true;
                    w.is_zoomed = false;
                }
                UnstashTargetState::Maximize => {
                    w.is_zoomed = true;
                    w.is_iconic = false;
                }
            }
        }
    }
    fn set_window_rect_visible(&self, hwnd: WindowHandle, visible_rect: Rect) {
        if let Some(w) = self.inner.borrow_mut().windows.get_mut(&hwnd) {
            w.rect = visible_rect;
        }
    }
    fn set_window_topmost(&self, hwnd: WindowHandle, topmost: bool) {
        self.inner.borrow_mut().topmost_calls.push((hwnd, topmost));
    }
    fn close_window(&self, hwnd: WindowHandle) {
        self.inner.borrow_mut().closed_window_calls.push(hwnd);
    }
    fn ensure_window_restored(&self, hwnd: WindowHandle) {
        let mut st = self.inner.borrow_mut();
        st.ensure_restored_calls.push(hwnd);
        if let Some(w) = st.windows.get_mut(&hwnd) {
            w.is_iconic = false;
        }
    }
    fn refresh_window_rendering(&self, hwnd: WindowHandle) {
        self.inner.borrow_mut().refresh_rendering_calls.push(hwnd);
    }
    fn is_window_zoomed(&self, hwnd: WindowHandle) -> bool {
        self.inner
            .borrow()
            .windows
            .get(&hwnd)
            .is_some_and(|w| w.is_zoomed)
    }
    fn park_and_cloak_window(&self, hwnd: WindowHandle, parked_origin: (i32, i32)) {
        if let Some(w) = self.inner.borrow_mut().windows.get_mut(&hwnd) {
            let width = w.rect.width();
            let height = w.rect.height();
            w.rect = Rect::new(
                parked_origin.0,
                parked_origin.1,
                parked_origin.0 + width,
                parked_origin.1 + height,
            );
            w.cloaked = true;
        }
    }
    fn uncloak_window(&self, hwnd: WindowHandle) {
        if let Some(w) = self.inner.borrow_mut().windows.get_mut(&hwnd) {
            w.cloaked = false;
        }
    }
    fn is_window_cloaked(&self, hwnd: WindowHandle) -> bool {
        self.inner
            .borrow()
            .windows
            .get(&hwnd)
            .is_some_and(|w| w.cloaked)
    }
    fn animate_transitions(
        &self,
        transitions: &[WindowTransition],
        _duration_ms: u32,
        style: AnimationStyle,
        is_opening: bool,
    ) {
        let mut st = self.inner.borrow_mut();
        st.animated_batches
            .push((transitions.to_vec(), style, is_opening));
        for tr in transitions {
            if let Some(w) = st.windows.get_mut(&tr.hwnd) {
                let width = w.rect.width();
                let height = w.rect.height();
                w.rect = Rect::new(
                    tr.end_visible_x,
                    tr.end_visible_y,
                    tr.end_visible_x + width,
                    tr.end_visible_y + height,
                );
                w.cloaked = !is_opening;
            }
        }
    }
    fn enumerate_monitors(&self) -> Vec<MonitorInfo> {
        self.inner.borrow().monitors.clone()
    }
    fn is_fullscreen_active(&self) -> bool {
        self.inner.borrow().fullscreen_active
    }
}

impl VirtualDesktopApi for MockPlatform {
    fn get_current_desktop_id(&self) -> Option<GUID> {
        Some(self.inner.borrow().current_desktop)
    }
    fn get_window_desktop_id(&self, hwnd: WindowHandle) -> Option<GUID> {
        self.inner.borrow().window_desktops.get(&hwnd).copied()
    }
    fn is_on_current_desktop(&self, hwnd: WindowHandle) -> bool {
        let st = self.inner.borrow();
        st.window_desktops.get(&hwnd) == Some(&st.current_desktop)
    }
    fn move_window_to_desktop(&self, hwnd: WindowHandle, desktop_id: &GUID) -> bool {
        self.inner
            .borrow_mut()
            .window_desktops
            .insert(hwnd, *desktop_id);
        true
    }
}

impl AppBarApi for MockPlatform {
    fn register_or_update_dock(
        &self,
        monitor_id: &str,
        edge: ScreenEdge,
        monitor_rect: Rect,
        thickness_px: i32,
        enable: bool,
    ) -> Option<Rect> {
        let mut st = self.inner.borrow_mut();
        st.appbars
            .push((monitor_id.to_string(), edge, thickness_px, enable));
        if enable {
            let work = st
                .monitors
                .iter()
                .find(|m| m.id == monitor_id)
                .map(|m| m.work_rect)
                .unwrap_or(monitor_rect);
            let reserved = match edge {
                ScreenEdge::Left => {
                    Rect::new(work.left, work.top, work.left + thickness_px, work.bottom)
                }
                ScreenEdge::Right => {
                    Rect::new(work.right - thickness_px, work.top, work.right, work.bottom)
                }
                ScreenEdge::Top => {
                    Rect::new(work.left, work.top, work.right, work.top + thickness_px)
                }
                ScreenEdge::Bottom => Rect::new(
                    work.left,
                    work.bottom - thickness_px,
                    work.right,
                    work.bottom,
                ),
            };
            if st.simulate_shrunk_work_area_on_dock
                && let Some(mon) = st.monitors.iter_mut().find(|m| m.id == monitor_id)
            {
                match edge {
                    ScreenEdge::Left => mon.work_rect.left = reserved.right,
                    ScreenEdge::Right => mon.work_rect.right = reserved.left,
                    ScreenEdge::Top => mon.work_rect.top = reserved.bottom,
                    ScreenEdge::Bottom => mon.work_rect.bottom = reserved.top,
                }
            }
            Some(reserved)
        } else {
            if st.simulate_shrunk_work_area_on_dock
                && let Some(mon) = st.monitors.iter_mut().find(|m| m.id == monitor_id)
            {
                mon.work_rect = Rect::new(
                    mon.rect.left,
                    mon.rect.top,
                    mon.rect.right,
                    mon.rect.bottom - 48,
                );
            }
            None
        }
    }
}

impl VisualOverlayApi for MockPlatform {
    fn emit_overlay_event(&self, event: PillOverlayEvent) {
        self.inner.borrow_mut().overlay_events.push(event);
    }
}

fn temp_manifest_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "edgestash-engine-test-{}-{name}.json",
        std::process::id()
    ))
}

#[test]
fn stash_summon_dismiss_and_delayed_focus_restore_lifecycle() {
    let manifest_path = temp_manifest_path("lifecycle");
    let _ = std::fs::remove_file(&manifest_path);

    let plat = MockPlatform::new();
    let clock = FakeClock::new(1000);
    let ide = plat.add_window(10, "VS Code", Rect::new(50, 50, 1400, 950));
    let notes = plat.add_window(20, "Obsidian", Rect::new(300, 100, 750, 800));
    plat.inner.borrow_mut().foreground = Some(ide);

    let cfg = EdgeStashConfig {
        dim_background: 30,
        ..Default::default()
    };
    let mut engine = EdgeStashEngine::new(
        plat.clone(),
        plat.clone(),
        plat.clone(),
        plat.clone(),
        &clock,
        cfg,
        manifest_path.clone(),
    );

    // 1. Stash Obsidian onto Right edge -> parked off-screen & cloaked, manifest written
    engine
        .stash_window(notes, ScreenEdge::Right, false)
        .expect("stash obsidian");
    assert!(plat.is_window_cloaked(notes));
    assert!(plat.get_window_rect(notes).unwrap().left >= 1920);
    assert!(manifest_path.exists());

    // 2. Summon Right edge -> opens with 28px top header strip, shows dim scrim, sets focus to notes
    assert!(
        engine
            .toggle_edge(Some("MON-1"), ScreenEdge::Right)
            .unwrap()
    );
    assert!(!plat.is_window_cloaked(notes));
    assert_eq!(
        plat.get_window_rect(notes).unwrap(),
        Rect::new(1920 - 450, 28, 1920, 1032)
    );
    assert_eq!(
        engine.header_rect_for_slot("MON-1", ScreenEdge::Right),
        Some(Rect::new(1920 - 450, 0, 1920, 28))
    );
    assert_eq!(plat.get_foreground_window(), Some(notes));
    assert!(plat.inner.borrow().overlay_events.iter().any(|e| {
        matches!(
            e,
            PillOverlayEvent::SetDimScrim {
                visible: true,
                opacity_percent: 30,
                ..
            }
        )
    }));

    // 3. Cursor enters drawer, then leaves past 24px margin -> waits 350ms -> hides & schedules focus restore (50ms)
    plat.inner.borrow_mut().cursor = (1600, 500);
    engine.tick(&clock);
    plat.inner.borrow_mut().cursor = (1000, 500);
    clock.advance(100);
    engine.tick(&clock);
    assert!(!plat.is_window_cloaked(notes));

    clock.advance(350);
    engine.tick(&clock);
    assert!(plat.is_window_cloaked(notes));
    assert!(plat.inner.borrow().restored_focus_calls.is_empty());
    assert!(engine.has_pending_focus_restore());

    // After 50ms focus_restore_delay_ms -> restores IDE focus cleanly!
    clock.advance(50);
    engine.tick(&clock);
    assert!(!engine.has_pending_focus_restore());
    assert_eq!(plat.inner.borrow().restored_focus_calls, vec![ide]);
    assert_eq!(plat.get_foreground_window(), Some(ide));

    // 4. Unstash restores original rect & clears manifest
    engine.unstash_window(notes).unwrap();
    assert!(!plat.is_window_cloaked(notes));
    assert_eq!(
        plat.get_window_rect(notes).unwrap(),
        Rect::new(300, 100, 750, 800)
    );
    let _ = std::fs::remove_file(&manifest_path);
}

#[test]
fn grouping_and_ungrouping_windows_animates_together_and_protects_intra_group_focus() {
    let manifest_path = temp_manifest_path("grouping");
    let _ = std::fs::remove_file(&manifest_path);

    let plat = MockPlatform::new();
    let clock = FakeClock::new(100);
    let ide = plat.add_window(10, "IDE", Rect::new(0, 0, 1000, 900));
    let notepad = plat.add_window(21, "Notepad", Rect::new(100, 100, 500, 600));
    let calc = plat.add_window(22, "Calculator", Rect::new(200, 200, 560, 700));
    plat.inner.borrow_mut().foreground = Some(ide);

    let mut engine = EdgeStashEngine::new(
        plat.clone(),
        plat.clone(),
        plat.clone(),
        plat.clone(),
        &clock,
        EdgeStashConfig::default(),
        manifest_path.clone(),
    );

    engine
        .stash_window(notepad, ScreenEdge::Right, false)
        .unwrap();
    engine.stash_window(calc, ScreenEdge::Right, true).unwrap();

    // Opening Right edge animates BOTH Notepad & Calc together below unified 28px header bar!
    engine
        .toggle_edge(Some("MON-1"), ScreenEdge::Right)
        .unwrap();
    let last_batch = plat.inner.borrow().animated_batches.last().unwrap().clone();
    assert_eq!(last_batch.0.len(), 2);
    assert_eq!(
        plat.get_window_rect(notepad).unwrap(),
        Rect::new(1920 - 400, 28, 1920, 530)
    );
    assert_eq!(
        plat.get_window_rect(calc).unwrap(),
        Rect::new(1920 - 360, 530, 1920, 1032)
    );

    // Switching focus from Notepad to Calculator keeps group open!
    engine.on_foreground_changed(calc);
    assert!(!plat.is_window_cloaked(notepad));
    assert!(!plat.is_window_cloaked(calc));

    // Clicking outside to IDE closes both together!
    engine.on_foreground_changed(ide);
    assert!(plat.is_window_cloaked(notepad));
    assert!(plat.is_window_cloaked(calc));

    // Ungroup Calculator -> creates 2 independent stacked slots on Right edge
    engine.ungroup_window(calc).unwrap();
    let status = engine.status_snapshot(1);
    let right_slots: Vec<_> = status
        .slots
        .iter()
        .filter(|s| s.edge == ScreenEdge::Right)
        .collect();
    assert_eq!(right_slots.len(), 2);

    // Cycling Right edge switches active slot
    engine
        .cycle_edge(Some("MON-1"), ScreenEdge::Right, 1)
        .unwrap();
    let status2 = engine.status_snapshot(1);
    assert!(
        status2
            .slots
            .iter()
            .any(|s| s.is_active_in_deck && s.windows[0].hwnd == calc.0)
    );
    let _ = std::fs::remove_file(&manifest_path);
}

#[test]
fn virtual_desktop_scoping_appbar_docking_and_destroyed_window_animation() {
    let manifest_path = temp_manifest_path("vdesktop");
    let _ = std::fs::remove_file(&manifest_path);

    let plat = MockPlatform::new();
    let clock = FakeClock::new(100);
    let d1 = plat.get_current_desktop_id().unwrap();
    let d2 = GUID::from_u128(0x9999_8888_7777_6666_5555_4444_3333_2222);

    let w_all = plat.add_window(31, "Spotify", Rect::new(100, 100, 500, 700));
    let w_pinned = plat.add_window(32, "Terminal", Rect::new(100, 100, 600, 700));

    let cfg = EdgeStashConfig {
        virtual_desktop_mode: VirtualDesktopMode::AllDesktops,
        ..Default::default()
    };
    let mut engine = EdgeStashEngine::new(
        plat.clone(),
        plat.clone(),
        plat.clone(),
        plat.clone(),
        &clock,
        cfg,
        manifest_path.clone(),
    );

    engine.stash_window(w_all, ScreenEdge::Left, false).unwrap();
    engine
        .stash_window(w_pinned, ScreenEdge::Bottom, false)
        .unwrap();
    engine
        .set_virtual_desktop_mode(w_pinned, VirtualDesktopMode::CurrentDesktopOnly)
        .unwrap();

    // Switch to Virtual Desktop 2
    plat.inner.borrow_mut().current_desktop = d2;

    // Pinned window on D1 is muted on D2!
    let err = engine
        .toggle_edge(Some("MON-1"), ScreenEdge::Bottom)
        .unwrap_err();
    assert_eq!(err.0, IpcErrorCode::NoSlotOnEdge);

    // Global window moves to D2 before opening!
    assert!(engine.toggle_edge(Some("MON-1"), ScreenEdge::Left).unwrap());
    assert_eq!(plat.get_window_desktop_id(w_all), Some(d2));

    // Switch Left edge to NativeWorkAreaDock -> registers AppBar & keeps open despite cursor outside
    engine
        .set_dock_mode(
            Some("MON-1"),
            ScreenEdge::Left,
            DockMode::NativeWorkAreaDock,
        )
        .unwrap();
    assert!(plat.inner.borrow().appbars.contains(&(
        "MON-1".to_string(),
        ScreenEdge::Left,
        400,
        true
    )));

    // External window destruction emits WindowClosedAnimation & purges slot + unregisters AppBar!
    plat.inner.borrow_mut().windows.remove(&w_all);
    engine.on_window_destroyed(w_all);
    assert!(plat.inner.borrow().overlay_events.iter().any(|e| {
        matches!(
            e,
            PillOverlayEvent::WindowClosedAnimation {
                edge: ScreenEdge::Left,
                closed_hwnd,
                ..
            } if *closed_hwnd == w_all
        )
    }));
    assert!(plat.inner.borrow().appbars.contains(&(
        "MON-1".to_string(),
        ScreenEdge::Left,
        400,
        false
    )));

    let _ = d1;
    let _ = std::fs::remove_file(&manifest_path);
}

#[test]
fn fullscreen_mute_single_floating_drawer_monitor_disconnect_and_resize_remembering() {
    let manifest_path = temp_manifest_path("misc");
    let _ = std::fs::remove_file(&manifest_path);

    let plat = MockPlatform::new();
    let second_mon = MonitorInfo {
        id: "MON-2".to_string(),
        rect: Rect::new(1920, 0, 3840, 1080),
        work_rect: Rect::new(1920, 0, 3840, 1032),
        dpi: 96,
        is_primary: false,
    };
    plat.inner.borrow_mut().monitors.push(second_mon);

    let clock = FakeClock::new(100);
    let w1 = plat.add_window(41, "Mail", Rect::new(50, 50, 450, 600));
    let w2 = plat.add_window(42, "Chat", Rect::new(2100, 50, 2600, 600));

    let mut engine = EdgeStashEngine::new(
        plat.clone(),
        plat.clone(),
        plat.clone(),
        plat.clone(),
        &clock,
        EdgeStashConfig::default(),
        manifest_path.clone(),
    );

    engine.stash_window(w1, ScreenEdge::Left, false).unwrap();
    engine.stash_window(w2, ScreenEdge::Right, false).unwrap();

    // Fullscreen blocks edge trigger activation, though explicit hotkey/IPC toggle works
    plat.inner.borrow_mut().fullscreen_active = true;
    assert!(!engine.on_edge_trigger("MON-1", ScreenEdge::Left));
    assert!(plat.is_window_cloaked(w1));

    plat.inner.borrow_mut().fullscreen_active = false;
    assert!(engine.on_edge_trigger("MON-1", ScreenEdge::Left));
    assert!(!plat.is_window_cloaked(w1));

    // User resizes w1 while open from width 400 -> 580; remembered for next open!
    plat.set_window_rect_visible(w1, Rect::new(0, 0, 580, 1032));
    engine.on_window_resized(w1);

    // Opening MON-2 Right closes MON-1 Left (single active floating drawer!)
    assert!(
        engine
            .toggle_edge(Some("MON-2"), ScreenEdge::Right)
            .unwrap()
    );
    assert!(plat.is_window_cloaked(w1));
    assert!(!plat.is_window_cloaked(w2));

    // Reopening MON-1 Left uses remembered width 580!
    assert!(engine.toggle_edge(Some("MON-1"), ScreenEdge::Left).unwrap());
    assert_eq!(plat.get_window_rect(w1).unwrap().width(), 580);

    // Disconnecting MON-2 migrates w2 cleanly onto MON-1 (primary)
    plat.inner.borrow_mut().monitors.truncate(1);
    engine.on_monitors_changed();
    let snap = engine.status_snapshot(1);
    assert!(snap.slots.iter().all(|s| s.monitor_id == "MON-1"));

    // Verify protected window rejected cleanly
    let shell = plat.add_window(99, "Shell", Rect::new(0, 0, 100, 100));
    plat.inner
        .borrow_mut()
        .windows
        .get_mut(&shell)
        .unwrap()
        .protected = true;
    let err = engine
        .stash_window(shell, ScreenEdge::Top, false)
        .unwrap_err();
    assert_eq!(err.0, IpcErrorCode::WindowProtected);

    let _ = HashSet::<isize>::new();
    let _ = std::fs::remove_file(&manifest_path);
}

#[test]
fn removing_earlier_slot_keeps_open_floating_drawer_index_consistent() {
    let manifest_path = temp_manifest_path("index-shift");
    let _ = std::fs::remove_file(&manifest_path);

    let plat = MockPlatform::new();
    let clock = FakeClock::new(100);
    let ide = plat.add_window(5, "IDE", Rect::new(0, 0, 1000, 900));
    let w1 = plat.add_window(51, "First", Rect::new(10, 10, 410, 500));
    let w2 = plat.add_window(52, "Second", Rect::new(20, 20, 420, 500));
    plat.inner.borrow_mut().foreground = Some(ide);

    let mut engine = EdgeStashEngine::new(
        plat.clone(),
        plat.clone(),
        plat.clone(),
        plat.clone(),
        &clock,
        EdgeStashConfig::default(),
        manifest_path.clone(),
    );

    engine.stash_window(w1, ScreenEdge::Right, false).unwrap();
    engine.stash_window(w2, ScreenEdge::Right, false).unwrap();
    // Active is now w2 (index 1); open it
    engine
        .toggle_edge(Some("MON-1"), ScreenEdge::Right)
        .unwrap();
    assert!(!plat.is_window_cloaked(w2));

    // Unstash w1 (index 0) while w2 (index 1) is open
    engine.unstash_window(w1).unwrap();

    // Clicking outside to IDE must STILL immediately close w2!
    engine.on_foreground_changed(ide);
    assert!(plat.is_window_cloaked(w2));
    let _ = std::fs::remove_file(&manifest_path);
}

#[test]
fn floating_overlay_promotes_topmost_on_open_and_native_dock_clears_topmost() {
    let manifest_path = temp_manifest_path("topmost-zorder");
    let _ = std::fs::remove_file(&manifest_path);

    let plat = MockPlatform::new();
    let clock = FakeClock::new(100);
    let w1 = plat.add_window(61, "OverlayWin", Rect::new(20, 20, 420, 620));

    let mut engine = EdgeStashEngine::new(
        plat.clone(),
        plat.clone(),
        plat.clone(),
        plat.clone(),
        &clock,
        EdgeStashConfig::default(),
        manifest_path.clone(),
    );

    engine.stash_window(w1, ScreenEdge::Right, false).unwrap();
    assert!(engine.on_edge_trigger("MON-1", ScreenEdge::Right));
    assert!(
        plat.inner.borrow().topmost_calls.contains(&(w1, true)),
        "FloatingOverlay open must promote window to HWND_TOPMOST"
    );

    engine
        .set_dock_mode(
            Some("MON-1"),
            ScreenEdge::Right,
            DockMode::NativeWorkAreaDock,
        )
        .unwrap();
    assert!(
        plat.inner.borrow().topmost_calls.contains(&(w1, false)),
        "NativeWorkAreaDock must clear HWND_TOPMOST"
    );

    let _ = std::fs::remove_file(&manifest_path);
}

#[test]
fn pin_modes_header_actions_app_icons_sync_and_drag_protection_lifecycle() {
    let manifest_path = temp_manifest_path("pins-drag-sync");
    let _ = std::fs::remove_file(&manifest_path);

    let plat = MockPlatform::new();
    plat.inner.borrow_mut().simulate_shrunk_work_area_on_dock = true;
    let clock = FakeClock::new(100);
    let ide = plat.add_window(70, "IDE", Rect::new(0, 0, 1200, 900));
    let notes = plat.add_window(71, "Notes", Rect::new(100, 100, 500, 600));
    let chat = plat.add_window(72, "Chat", Rect::new(120, 120, 520, 620));
    let music = plat.add_window(73, "Music", Rect::new(140, 140, 540, 640));
    let side = plat.add_window(74, "SideLeft", Rect::new(20, 20, 380, 600));
    plat.inner.borrow_mut().foreground = Some(ide);

    let cfg = EdgeStashConfig {
        dim_background: 35,
        ..Default::default()
    };
    let mut engine = EdgeStashEngine::new(
        plat.clone(),
        plat.clone(),
        plat.clone(),
        plat.clone(),
        &clock,
        cfg,
        manifest_path.clone(),
    );

    // Group Notes+Chat in slot 0 and stash Music in slot 1 on Right; stash SideLeft on Left
    engine
        .stash_window(notes, ScreenEdge::Right, false)
        .unwrap();
    engine.stash_window(chat, ScreenEdge::Right, true).unwrap();
    engine
        .stash_window(music, ScreenEdge::Right, false)
        .unwrap();
    engine.stash_window(side, ScreenEdge::Left, false).unwrap();

    // SyncState emits 1:1 per-slot representative handle & title matching slot_count = 2 on Right!
    let last_right_sync = plat
        .inner
        .borrow()
        .overlay_events
        .iter()
        .rev()
        .find(|e| {
            matches!(
                e,
                PillOverlayEvent::SyncState {
                    edge: ScreenEdge::Right,
                    ..
                }
            )
        })
        .cloned()
        .expect("Right SyncState emitted");
    if let PillOverlayEvent::SyncState {
        slot_count,
        window_handles,
        slot_titles,
        ..
    } = last_right_sync
    {
        assert_eq!(slot_count, 2);
        assert_eq!(window_handles, vec![notes, music]);
        assert_eq!(slot_titles, vec!["Notes".to_string(), "Music".to_string()]);
    }

    // Select & open slot 0 (Notes+Chat) on Right edge directly (clicking first icon in AppIconsStack)
    engine
        .select_and_open_slot(Some("MON-1"), ScreenEdge::Right, 0, true)
        .unwrap();
    assert!(!plat.is_window_cloaked(notes));
    assert!(plat.is_window_cloaked(music));

    // Toggle Pin via left-click: default goes FloatingPinned, hides dim scrim, keeps HWND_TOPMOST!
    let pinned_mode = engine
        .toggle_pin_active(Some("MON-1"), ScreenEdge::Right)
        .unwrap();
    assert_eq!(pinned_mode, DockMode::FloatingPinned);
    assert!(matches!(
        plat.inner.borrow().overlay_events.last(),
        Some(PillOverlayEvent::ShowDrawerHeader {
            dock_mode: DockMode::FloatingPinned,
            ..
        }) | Some(PillOverlayEvent::SyncState { .. })
    ));
    assert!(plat.inner.borrow().overlay_events.iter().rev().any(|e| {
        matches!(
            e,
            PillOverlayEvent::SetDimScrim {
                visible: false,
                opacity_percent: 0,
                ..
            }
        )
    }));

    // FloatingPinned ignores mouse-leave AND external foreground focus loss!
    plat.inner.borrow_mut().cursor = (1600, 500);
    engine.tick(&clock);
    plat.inner.borrow_mut().cursor = (200, 500);
    clock.advance(2000);
    engine.tick(&clock);
    engine.on_foreground_changed(ide);
    assert!(!plat.is_window_cloaked(notes));

    // Opening another FloatingOverlay on Left edge must NOT auto-close FloatingPinned on Right!
    engine.toggle_edge(Some("MON-1"), ScreenEdge::Left).unwrap();
    assert!(!plat.is_window_cloaked(side));
    assert!(!plat.is_window_cloaked(notes));

    // Switch Right edge pin via right-click to NativeWorkAreaDock: shrinks work area yet stays flush at [1520..1920]!
    engine
        .set_dock_mode(
            Some("MON-1"),
            ScreenEdge::Right,
            DockMode::NativeWorkAreaDock,
        )
        .unwrap();
    assert_eq!(
        engine.header_rect_for_slot("MON-1", ScreenEdge::Right),
        Some(Rect::new(1520, 0, 1920, 28))
    );
    assert_eq!(
        plat.get_window_rect(notes).unwrap(),
        Rect::new(1520, 28, 1920, 530)
    );

    // Header bar Close button (left-click) collapses drawer; CloseApp sends WM_CLOSE
    engine
        .close_slot_drawer(Some("MON-1"), ScreenEdge::Left)
        .unwrap();
    assert!(plat.is_window_cloaked(side));

    engine.toggle_edge(Some("MON-1"), ScreenEdge::Left).unwrap();
    engine
        .close_slot_app(Some("MON-1"), ScreenEdge::Left)
        .unwrap();
    assert_eq!(plat.inner.borrow().closed_window_calls, vec![side]);

    // Drag-over-pill opens without stealing focus and prevents auto-hide until drag finishes
    let dwin = plat.add_window(75, "DropTarget", Rect::new(40, 40, 440, 540));
    engine.stash_window(dwin, ScreenEdge::Left, false).unwrap();
    plat.inner.borrow_mut().foreground = Some(ide);
    engine.set_drag_active(true);
    assert!(engine.on_drag_over_pill_trigger("MON-1", ScreenEdge::Left));
    assert!(!plat.is_window_cloaked(dwin));
    assert_eq!(plat.get_foreground_window(), Some(ide));

    plat.inner.borrow_mut().cursor = (100, 500);
    engine.tick(&clock);
    plat.inner.borrow_mut().cursor = (900, 500);
    clock.advance(2000);
    engine.tick(&clock);
    assert!(
        !plat.is_window_cloaked(dwin),
        "active drag must keep drawer open"
    );

    engine.set_drag_active(false);
    engine.tick(&clock);
    clock.advance(400);
    engine.tick(&clock);
    assert!(
        plat.is_window_cloaked(dwin),
        "releasing drag allows normal mouse-leave close"
    );

    let _ = std::fs::remove_file(&manifest_path);
}

#[test]
fn locked_stashed_window_snaps_back_on_move_while_allowing_resize_and_churn_zones_dispatch() {
    let plat = MockPlatform::new();
    let clock = FakeClock::new(1000);
    let manifest_path = std::env::temp_dir().join("edgestash_lock_churn_manifest.json");
    let _ = std::fs::remove_file(&manifest_path);

    let mut cfg = EdgeStashConfig::default();
    assert!(cfg.lock_stashed_position);
    cfg.churn_zones.set(
        ChurnZoneId::BottomRightUp,
        ChurnZoneAction::SpecificApp {
            matcher: "spotify".to_string(),
        },
    );
    cfg.churn_zones.set(
        ChurnZoneId::TopRightDown,
        ChurnZoneAction::DefaultLastOpened,
    );
    cfg.churn_zones
        .set(ChurnZoneId::TopLeftDown, ChurnZoneAction::Disabled);

    let spotify = plat.add_window_with_process(
        201,
        "Spotify Free",
        "spotify.exe",
        Rect::new(100, 100, 500, 600),
    );
    let notes = plat.add_window_with_process(
        202,
        "Notes.txt",
        "notepad.exe",
        Rect::new(200, 200, 620, 700),
    );
    let left_term =
        plat.add_window_with_process(203, "PowerShell", "pwsh.exe", Rect::new(0, 0, 400, 700));

    let mut engine = EdgeStashEngine::new(
        plat.clone(),
        plat.clone(),
        plat.clone(),
        plat.clone(),
        &clock,
        cfg.clone(),
        manifest_path.clone(),
    );

    engine
        .stash_window(spotify, ScreenEdge::Right, false)
        .unwrap();
    engine
        .stash_window(notes, ScreenEdge::Right, false)
        .unwrap();
    engine
        .stash_window(left_term, ScreenEdge::Left, false)
        .unwrap();

    // Open Notes via normal selection -> sets default last opened = Notes
    engine
        .select_and_open_slot(Some("MON-1"), ScreenEdge::Right, 1, false)
        .unwrap();
    let initial_notes_rect = plat.get_window_rect(notes).unwrap();
    assert_eq!(initial_notes_rect, Rect::new(1500, 28, 1920, 1032));

    // Problem 3: During live drag, on_window_moved_or_resized(notes, true) does NOT snap back (eliminates stutter)!
    plat.inner
        .borrow_mut()
        .windows
        .get_mut(&notes)
        .unwrap()
        .rect = Rect::new(1840, 28, 2260, 1032);
    assert!(!engine.on_window_moved_or_resized(notes, true));
    assert_eq!(
        plat.get_window_rect(notes).unwrap(),
        Rect::new(1840, 28, 2260, 1032)
    );

    // Dropping near edge (within 100px: distance = 80px) on mouse release snaps it back to initial_notes_rect!
    plat.inner
        .borrow_mut()
        .windows
        .get_mut(&notes)
        .unwrap()
        .rect = Rect::new(1420, 28, 1840, 1032);
    assert!(engine.on_window_moved_or_resized(notes, false));
    assert_eq!(plat.get_window_rect(notes).unwrap(), initial_notes_rect);

    // Resizing inner left edge wider (width 420 -> 540) keeps it docked at right=1920 with updated width 540!
    plat.inner
        .borrow_mut()
        .windows
        .get_mut(&notes)
        .unwrap()
        .rect = Rect::new(1380, 28, 1920, 1032);
    assert!(engine.on_window_moved_or_resized(notes, false));
    assert_eq!(
        plat.get_window_rect(notes).unwrap(),
        Rect::new(1380, 28, 1920, 1032)
    );

    // Dragging and dropping far into screen (> 100px from bezel, e.g. dropped at x=900) automatically unstashes it!
    plat.inner
        .borrow_mut()
        .windows
        .get_mut(&notes)
        .unwrap()
        .rect = Rect::new(900, 100, 1440, 1104);
    assert!(engine.on_window_moved_or_resized(notes, false));
    assert!(!engine.is_window_stashed(notes));

    // Re-stash notes so later churn zone tests have their default target
    engine
        .stash_window(notes, ScreenEdge::Right, false)
        .unwrap();
    engine
        .select_and_open_slot(Some("MON-1"), ScreenEdge::Right, 1, false)
        .unwrap();

    // Turning lock_stashed_position = false stops move snap-back entirely
    let mut unlocked = engine.config().clone();
    unlocked.lock_stashed_position = false;
    engine.update_config(unlocked);
    plat.inner
        .borrow_mut()
        .windows
        .get_mut(&notes)
        .unwrap()
        .rect = Rect::new(900, 100, 1440, 1104);
    assert!(!engine.on_window_moved_or_resized(notes, true));
    assert_eq!(
        plat.get_window_rect(notes).unwrap(),
        Rect::new(900, 100, 1440, 1104)
    );

    engine
        .close_slot_drawer(Some("MON-1"), ScreenEdge::Right)
        .unwrap();

    // Feature #7: BottomRightUp always opens Spotify without clobbering Default (Notes)!
    assert_eq!(
        engine.on_churn_zone_trigger("MON-1", ChurnZoneId::BottomRightUp),
        Some(("MON-1".to_string(), ScreenEdge::Right))
    );
    assert!(!plat.is_window_cloaked(spotify));
    assert!(plat.is_window_cloaked(notes));

    // Close Spotify, churn TopRightDown (DefaultLastOpened) -> opens Notes again!
    engine
        .close_slot_drawer(Some("MON-1"), ScreenEdge::Right)
        .unwrap();
    assert_eq!(
        engine.on_churn_zone_trigger("MON-1", ChurnZoneId::TopRightDown),
        Some(("MON-1".to_string(), ScreenEdge::Right))
    );
    assert!(!plat.is_window_cloaked(notes));

    // Disabled churn zone never triggers even when slots exist on Left
    assert_eq!(
        engine.on_churn_zone_trigger("MON-1", ChurnZoneId::TopLeftDown),
        None
    );

    // If assigned SpecificApp (Spotify) closes/unstashes, BottomRightUp does nothing (never opens Notes)
    engine
        .close_slot_drawer(Some("MON-1"), ScreenEdge::Right)
        .unwrap();
    engine.on_window_destroyed(spotify);
    assert_eq!(
        engine.on_churn_zone_trigger("MON-1", ChurnZoneId::BottomRightUp),
        None
    );
    assert!(plat.is_window_cloaked(notes));

    let _ = std::fs::remove_file(&manifest_path);
}

#[test]
fn window_attention_detection_prioritization_and_focus_clearing_lifecycle() {
    let manifest_path = temp_manifest_path("attention-lifecycle");
    let plat = MockPlatform::new();
    let clock = FakeClock::new(100);

    let slack = plat.add_window(101, "Slack", Rect::new(100, 100, 500, 600));
    let messenger = plat.add_window(102, "Messenger", Rect::new(200, 100, 600, 600));

    let cfg = EdgeStashConfig {
        prioritize_attention_on_summon: true,
        ..Default::default()
    };

    let mut engine = EdgeStashEngine::new(
        plat.clone(),
        plat.clone(),
        plat.clone(),
        plat.clone(),
        &clock,
        cfg,
        manifest_path.clone(),
    );

    // Stash Slack and Messenger onto Right edge as separate slots
    engine
        .stash_window(slack, ScreenEdge::Right, false)
        .unwrap();
    engine
        .stash_window(messenger, ScreenEdge::Right, false)
        .unwrap();

    let snap_init = engine.status_snapshot(1);
    assert_eq!(snap_init.slots.len(), 2);
    assert!(!snap_init.slots[0].needs_attention);
    assert!(!snap_init.slots[1].needs_attention);

    // External event: Messenger calls FlashWindowEx -> HSHELL_FLASH
    let att = engine.handle_window_attention(messenger, 200);
    assert_eq!(att, Some(("MON-1".to_string(), ScreenEdge::Right, 1)));

    // Status snapshot reflects attention on Messenger slot
    let snap_att = engine.status_snapshot(1);
    assert!(!snap_att.slots[0].needs_attention);
    assert!(snap_att.slots[1].needs_attention);
    assert!(snap_att.slots[1].windows[0].needs_attention);

    // Verify PillOverlayEvent was emitted with needs_attention and attention_count == 1
    let last_event = plat.inner.borrow().overlay_events.last().cloned().unwrap();
    match last_event {
        PillOverlayEvent::SyncState {
            needs_attention,
            attention_count,
            ..
        } => {
            assert_eq!(needs_attention, vec![false, true]);
            assert_eq!(attention_count, 1);
        }
        other => panic!("expected SyncState, got {other:?}"),
    }

    // Edge summon (bezel push or toggle) prioritizes the attention slot (Messenger) over Slot 0 (Slack)!
    assert!(
        engine
            .toggle_edge(Some("MON-1"), ScreenEdge::Right)
            .unwrap()
    );
    assert!(!plat.is_window_cloaked(messenger));
    assert!(plat.is_window_cloaked(slack));

    // Focus transfer to Messenger clears its attention alert cleanly
    engine.on_foreground_changed(messenger);
    let snap_cleared = engine.status_snapshot(1);
    assert!(!snap_cleared.slots[1].needs_attention);

    let _ = std::fs::remove_file(&manifest_path);
}

#[test]
fn stashed_window_minimize_click_unstashes_and_system_minimize_auto_restores_on_summon() {
    let manifest_path = temp_manifest_path("minimize-explorer-tests");
    let plat = MockPlatform::new();
    let clock = FakeClock::new(100);

    let explorer = plat.add_window_with_process(
        201,
        "File Explorer",
        "explorer.exe",
        Rect::new(200, 200, 1000, 800),
    );

    let cfg = EdgeStashConfig::default();
    let mut engine = EdgeStashEngine::new(
        plat.clone(),
        plat.clone(),
        plat.clone(),
        plat.clone(),
        &clock,
        cfg,
        manifest_path.clone(),
    );

    // 1. Stash File Explorer to Right edge (parks and cloaks window into stash)
    engine
        .stash_window(explorer, ScreenEdge::Right, false)
        .unwrap();
    assert!(engine.is_window_stashed(explorer));
    assert!(!engine.has_any_open_slot());

    // 2. Simulate System Minimize (Win+D / Win+M): window becomes iconic while stashed
    plat.inner
        .borrow_mut()
        .windows
        .get_mut(&explorer)
        .unwrap()
        .is_iconic = true;
    assert!(plat.inner.borrow().windows[&explorer].is_iconic);

    // 3. User summons stashed drawer: EdgeStash must auto-restore iconic window and refresh rendering
    assert!(engine.on_edge_trigger("MON-1", ScreenEdge::Right));
    assert!(engine.has_any_open_slot());

    // Verify ensure_window_restored cleared the iconic state
    assert!(!plat.inner.borrow().windows[&explorer].is_iconic);
    assert!(
        plat.inner
            .borrow()
            .ensure_restored_calls
            .contains(&explorer)
    );
    assert!(
        plat.inner
            .borrow()
            .refresh_rendering_calls
            .contains(&explorer)
    );

    // 4. Simulate user clicking Minimize button: un-stashes the window and leaves it minimized
    assert!(
        engine
            .unstash_window_with_state(explorer, UnstashTargetState::Minimize)
            .is_ok()
    );
    assert!(!engine.is_window_stashed(explorer));
    assert!(!engine.has_any_open_slot());
    assert!(plat.inner.borrow().windows[&explorer].is_iconic);

    let _ = std::fs::remove_file(&manifest_path);
}

#[test]
fn stashed_window_maximize_unstashes_and_maximizes() {
    let manifest_path = temp_manifest_path("maximize-unstash-tests");
    let plat = MockPlatform::new();
    let clock = FakeClock::new(100);

    let editor = plat.add_window_with_process(
        301,
        "Code Editor",
        "code.exe",
        Rect::new(100, 100, 800, 600),
    );

    let cfg = EdgeStashConfig::default();
    let mut engine = EdgeStashEngine::new(
        plat.clone(),
        plat.clone(),
        plat.clone(),
        plat.clone(),
        &clock,
        cfg,
        manifest_path.clone(),
    );

    // 1. Stash editor to Right edge and open drawer
    engine
        .stash_window(editor, ScreenEdge::Right, false)
        .unwrap();
    assert!(engine.on_edge_trigger("MON-1", ScreenEdge::Right));
    assert!(engine.is_window_stashed_and_open(editor));

    // 2. User clicks Maximize button on stashed window (window becomes zoomed)
    plat.inner
        .borrow_mut()
        .windows
        .get_mut(&editor)
        .unwrap()
        .is_zoomed = true;
    plat.inner
        .borrow_mut()
        .windows
        .get_mut(&editor)
        .unwrap()
        .rect = Rect::new(0, 0, 1920, 1032);

    // 3. Movement/resize event fires: window must automatically un-stash!
    assert!(engine.on_window_moved_or_resized(editor, true));
    // Verify window is un-stashed AND its maximized state is preserved!
    assert!(!engine.is_window_stashed(editor));
    assert!(!engine.has_any_open_slot());
    assert!(plat.inner.borrow().windows[&editor].is_zoomed);

    let _ = std::fs::remove_file(&manifest_path);
}

#[test]
fn active_foreground_window_never_triggers_attention_flash() {
    let manifest_path = temp_manifest_path("foreground-attention-suppression");
    let plat = MockPlatform::new();
    let clock = FakeClock::new(100);

    let explorer = plat.add_window_with_process(
        401,
        "File Explorer",
        "explorer.exe",
        Rect::new(200, 200, 1000, 800),
    );

    let cfg = EdgeStashConfig::default();
    let mut engine = EdgeStashEngine::new(
        plat.clone(),
        plat.clone(),
        plat.clone(),
        plat.clone(),
        &clock,
        cfg,
        manifest_path.clone(),
    );

    engine
        .stash_window(explorer, ScreenEdge::Right, false)
        .unwrap();
    assert!(engine.on_edge_trigger("MON-1", ScreenEdge::Right));

    // User is actively focused in Explorer (foreground window)
    plat.set_foreground_window(explorer);
    assert_eq!(plat.get_foreground_window(), Some(explorer));

    // Even if attention is delivered (e.g. folder change / redraw), it must be rejected!
    let res = engine.handle_window_attention(explorer, clock.now_ms());
    assert_eq!(
        res, None,
        "Foreground active window must NEVER trigger attention"
    );

    let snap = engine.status_snapshot(1);
    assert!(!snap.slots[0].needs_attention);
    assert!(!snap.slots[0].windows[0].needs_attention);

    let _ = std::fs::remove_file(&manifest_path);
}

#[test]
fn auto_focus_setting_controls_window_activation_and_mouse_leave_restoration() {
    let manifest_path = temp_manifest_path("auto-focus-setting-tests");
    let plat = MockPlatform::new();
    let clock = FakeClock::new(100);

    let messenger = plat.add_window(501, "Messenger", Rect::new(100, 100, 500, 700));
    let browser = plat.add_window(502, "Chrome", Rect::new(50, 50, 1200, 800));
    plat.inner.borrow_mut().foreground = Some(browser);

    // Test 1: auto_focus_on_open = false -> Opening stash leaves browser focused!
    let mut cfg = EdgeStashConfig {
        auto_focus_on_open: false,
        ..Default::default()
    };
    let mut engine = EdgeStashEngine::new(
        plat.clone(),
        plat.clone(),
        plat.clone(),
        plat.clone(),
        &clock,
        cfg.clone(),
        manifest_path.clone(),
    );

    engine
        .stash_window(messenger, ScreenEdge::Right, false)
        .unwrap();
    assert!(engine.on_edge_trigger("MON-1", ScreenEdge::Right));
    assert_eq!(
        plat.get_foreground_window(),
        Some(browser),
        "Foreground must remain Chrome when auto_focus is disabled"
    );

    // Close drawer
    engine
        .close_slot_drawer(Some("MON-1"), ScreenEdge::Right)
        .unwrap();

    // Test 2: auto_focus_on_open = true -> Opening stash transfers focus to Messenger!
    cfg.auto_focus_on_open = true;
    cfg.mouse_leave_delay_ms = 100;
    cfg.focus_restore_delay_ms = 50;
    engine.update_config(cfg);

    plat.inner.borrow_mut().foreground = Some(browser);
    assert!(engine.on_edge_trigger("MON-1", ScreenEdge::Right));
    assert_eq!(
        plat.get_foreground_window(),
        Some(messenger),
        "Foreground must transfer to Messenger when auto_focus is enabled"
    );

    // Move cursor into drawer, then move out -> after delay, drawer hides and focus restores to browser!
    plat.inner.borrow_mut().cursor = (1920 - 200, 300);
    engine.tick(&clock);

    // Move cursor outside drawer -> starts countdown
    plat.inner.borrow_mut().cursor = (500, 500);
    engine.tick(&clock);

    // Advance past mouse_leave_delay_ms (100ms) -> drawer hides and pending focus restore is set
    clock.advance(100);
    engine.tick(&clock);
    assert!(!engine.has_any_open_slot());
    assert!(engine.has_pending_focus_restore());

    // Advance 50ms -> focus restored to Chrome
    clock.advance(50);
    engine.tick(&clock);
    assert!(!engine.has_pending_focus_restore());
    assert_eq!(
        plat.get_foreground_window(),
        Some(browser),
        "Foreground must restore to Chrome when mouse leaves"
    );

    let _ = std::fs::remove_file(&manifest_path);
}

#[test]
fn stash_window_and_bind_churn_zone_sets_corner_trigger_binding() {
    let plat = MockPlatform::new();
    let clock = FakeClock::new(1000);
    let manifest_path = temp_manifest_path("corner-binding-tests");
    let mut engine = EdgeStashEngine::new(
        plat.clone(),
        plat.clone(),
        plat.clone(),
        plat.clone(),
        &clock,
        EdgeStashConfig::default(),
        manifest_path.clone(),
    );

    let messenger = plat.add_window_with_process(
        101,
        "Messenger",
        "messenger.exe",
        Rect::new(200, 200, 800, 700),
    );

    // Dropping into TopRight corner zone -> stashes on Right edge and binds TopRightDown to messenger.exe!
    engine
        .stash_window_and_bind_churn_zone(messenger, ChurnZoneId::TopRightDown)
        .unwrap();

    assert!(engine.is_window_stashed(messenger));
    assert!(engine.has_slots_on_edge("MON-1", ScreenEdge::Right));

    // Check that config churn_zones was updated with SpecificApp for messenger.exe
    match engine.config().churn_zones.top_right_down {
        ChurnZoneAction::SpecificApp { ref matcher } => {
            assert_eq!(matcher, "messenger.exe");
        }
        _ => panic!("Expected TopRightDown churn zone to be bound to messenger.exe"),
    }

    // Corner slide churn on TopRightDown opens this slot!
    let opened = engine.on_churn_zone_trigger("MON-1", ChurnZoneId::TopRightDown);
    assert_eq!(opened, Some(("MON-1".to_string(), ScreenEdge::Right)));
    assert!(engine.is_edge_open("MON-1", ScreenEdge::Right));

    let _ = std::fs::remove_file(&manifest_path);
}

#[test]
fn stash_window_and_bind_churn_zone_persists_config_to_disk() {
    let plat = MockPlatform::new();
    let clock = FakeClock::new(1000);
    let manifest_path = temp_manifest_path("corner-binding-disk-manifest");
    let config_dir = std::env::temp_dir().join(format!("corner_bind_cfg_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&config_dir);
    let config_path = config_dir.join("config.toml");
    EdgeStashConfig::default().save_to_at(&config_path).unwrap();

    let mut engine = EdgeStashEngine::new(
        plat.clone(),
        plat.clone(),
        plat.clone(),
        plat.clone(),
        &clock,
        EdgeStashConfig::default(),
        manifest_path.clone(),
    );
    engine.set_config_path(config_path.clone());

    let spotify =
        plat.add_window_with_process(201, "Spotify", "spotify.exe", Rect::new(100, 100, 500, 500));

    engine
        .stash_window_and_bind_churn_zone(spotify, ChurnZoneId::BottomRightUp)
        .unwrap();

    // Verify config.toml on disk was immediately updated!
    let reloaded = EdgeStashConfig::load_or_create_at(&config_path).unwrap();
    assert_eq!(
        reloaded.churn_zones.bottom_right_up,
        ChurnZoneAction::SpecificApp {
            matcher: "spotify.exe".to_string(),
        }
    );

    let _ = std::fs::remove_file(&manifest_path);
    let _ = std::fs::remove_dir_all(&config_dir);
}

#[test]
fn normal_stash_after_corner_churn_zone_trigger_reveals_normal_default_view() {
    let plat = MockPlatform::new();
    let clock = FakeClock::new(1000);
    let manifest_path = temp_manifest_path("normal-stash-corner-churn");
    let mut engine = EdgeStashEngine::new(
        plat.clone(),
        plat.clone(),
        plat.clone(),
        plat.clone(),
        &clock,
        EdgeStashConfig::default(),
        manifest_path.clone(),
    );

    let calc =
        plat.add_window_with_process(101, "Calculator", "calc.exe", Rect::new(200, 200, 600, 600));
    let explorer = plat.add_window_with_process(
        102,
        "File Explorer",
        "explorer.exe",
        Rect::new(300, 300, 900, 800),
    );

    // 1. Drop Calculator into TopRight corner zone (binds TopRightDown to calc.exe)
    engine
        .stash_window_and_bind_churn_zone(calc, ChurnZoneId::TopRightDown)
        .unwrap();

    // 2. Drop Explorer into the normal right edge (middle zone)
    engine
        .stash_window(explorer, ScreenEdge::Right, false)
        .unwrap();

    // 3. Trigger corner churn zone TopRightDown (should open calc)
    let opened_corner = engine.on_churn_zone_trigger("MON-1", ChurnZoneId::TopRightDown);
    assert_eq!(
        opened_corner,
        Some(("MON-1".to_string(), ScreenEdge::Right))
    );
    assert!(engine.is_edge_open("MON-1", ScreenEdge::Right));
    assert!(
        engine.is_window_stashed_and_open(calc),
        "Calc must open from corner trigger"
    );
    assert!(
        !engine.is_window_stashed_and_open(explorer),
        "Explorer must NOT open from calc corner trigger"
    );

    // 4. Close the drawer (e.g. mouse leaves or dismissal)
    engine
        .close_slot_drawer(Some("MON-1"), ScreenEdge::Right)
        .unwrap();
    assert!(!engine.is_edge_open("MON-1", ScreenEdge::Right));

    // 5. Trigger normal edge (e.g. EdgeBump or EdgeHover)
    let triggered_normal = engine.on_edge_trigger("MON-1", ScreenEdge::Right);
    assert!(triggered_normal);
    assert!(engine.is_edge_open("MON-1", ScreenEdge::Right));
    assert!(
        engine.is_window_stashed_and_open(explorer),
        "Normal default view (Explorer) must be revealed, NOT hijacked by Calc"
    );
    assert!(
        !engine.is_window_stashed_and_open(calc),
        "Calc should remain cloaked during normal edge trigger"
    );

    // 6. Close drawer again
    engine
        .close_slot_drawer(Some("MON-1"), ScreenEdge::Right)
        .unwrap();

    // 7. Trigger a DefaultLastOpened corner zone (BottomRightUp)
    let opened_default_corner = engine.on_churn_zone_trigger("MON-1", ChurnZoneId::BottomRightUp);
    assert_eq!(
        opened_default_corner,
        Some(("MON-1".to_string(), ScreenEdge::Right))
    );
    assert!(
        engine.is_window_stashed_and_open(explorer),
        "Default corner zone must also reveal normal default view (Explorer)"
    );

    let _ = std::fs::remove_file(&manifest_path);
}

#[test]
fn reapply_taskbar_deletion_deletes_tabs_for_all_stashed_windows() {
    let plat = MockPlatform::new();
    let clock = FakeClock::new(1000);
    let manifest_path = temp_manifest_path("reapply-taskbar-tests");
    let mut engine = EdgeStashEngine::new(
        plat.clone(),
        plat.clone(),
        plat.clone(),
        plat.clone(),
        &clock,
        EdgeStashConfig::default(),
        manifest_path.clone(),
    );

    let w1 = plat.add_window(101, "App1", Rect::new(100, 100, 500, 400));
    let w2 = plat.add_window(102, "App2", Rect::new(200, 200, 600, 500));
    let w3 = plat.add_window(103, "App3", Rect::new(300, 300, 700, 600));

    engine.stash_window(w1, ScreenEdge::Left, false).unwrap();
    engine.stash_window(w2, ScreenEdge::Right, false).unwrap();
    engine.stash_window(w3, ScreenEdge::Right, false).unwrap();

    plat.inner.borrow_mut().hide_from_taskbar_calls.clear();

    // Explorer restarts -> daemon calls reapply_taskbar_deletion()
    engine.reapply_taskbar_deletion();

    let calls = plat.inner.borrow().hide_from_taskbar_calls.clone();
    assert_eq!(calls.len(), 3);
    assert!(calls.contains(&w1));
    assert!(calls.contains(&w2));
    assert!(calls.contains(&w3));

    let _ = std::fs::remove_file(&manifest_path);
}

#[test]
fn stashed_window_docking_rect_preserved_on_reveal() {
    let plat = MockPlatform::new();
    let clock = FakeClock::new(1000);
    let manifest_path = temp_manifest_path("docking-rect-preserve-tests");
    let mut engine = EdgeStashEngine::new(
        plat.clone(),
        plat.clone(),
        plat.clone(),
        plat.clone(),
        &clock,
        EdgeStashConfig {
            auto_focus_on_open: true,
            ..Default::default()
        },
        manifest_path.clone(),
    );

    // Window was in center of screen: [600, 200, 1400, 800]
    let win = plat.add_window(501, "Editor", Rect::new(600, 200, 1400, 800));
    engine.stash_window(win, ScreenEdge::Right, false).unwrap();

    // While closed, parked outside screen
    assert!(!engine.is_edge_open("MON-1", ScreenEdge::Right));

    // Reveal the stash (e.g. edge trigger)
    assert!(engine.on_edge_trigger("MON-1", ScreenEdge::Right));
    assert!(engine.is_edge_open("MON-1", ScreenEdge::Right));

    // Final window rect must be docked against right bezel, NOT restored to center [600, 200, ...]!
    let final_rect = plat.get_window_rect(win).unwrap();
    assert_eq!(
        final_rect.right, 1920,
        "Must be docked to right screen bezel"
    );
    assert_eq!(final_rect.top, 28, "Top must be below 28px drawer header");

    let _ = std::fs::remove_file(&manifest_path);
}
