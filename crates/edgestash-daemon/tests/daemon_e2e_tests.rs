use std::sync::{Mutex, OnceLock, mpsc};
use std::thread;
use std::time::Duration;

use edgestash_daemon::daemon::{DaemonLaunchOptions, run_daemon_with_options};
use edgestash_daemon::geometry::{HeaderButtonKind, header_button_rects};
use edgestash_ipc::config::EdgeStashConfig;
use edgestash_ipc::pipe::PipeClient;
use edgestash_ipc::protocol::{IpcRequest, IpcResponse};
use edgestash_ipc::types::{DockMode, ScreenEdge};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DispatchMessageW, EnumWindows, GWL_EXSTYLE, GWLP_USERDATA,
    GetClassNameW, GetMessageW, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId,
    IsWindowVisible, MSG, PostMessageW, PostThreadMessageW, TranslateMessage, WM_APP, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_QUIT, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_OVERLAPPEDWINDOW, WindowFromPoint,
};
use windows::core::w;

/// Owns test windows on a dedicated message-pumping UI thread (just like a real target app),
/// preventing cross-thread `SendMessage` deadlocks while the test main thread blocks on IPC.
struct PumpingTestWindows {
    hwnds: Vec<isize>,
    thread_id: u32,
    join: Option<thread::JoinHandle<()>>,
}

impl PumpingTestWindows {
    fn spawn(titles: &[&str]) -> Self {
        let titles_owned: Vec<String> = titles.iter().map(|s| (*s).to_string()).collect();
        let (tx, rx) = mpsc::channel();
        let join = thread::spawn(move || {
            let tid = unsafe { GetCurrentThreadId() };
            let mut hwnds = Vec::new();
            let mut raw_hwnds = Vec::new();
            for title in titles_owned {
                let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
                let hwnd = unsafe {
                    CreateWindowExW(
                        WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                        w!("STATIC"),
                        windows::core::PCWSTR(wide.as_ptr()),
                        WS_OVERLAPPEDWINDOW,
                        -32000,
                        -32000,
                        420,
                        320,
                        None,
                        None,
                        None,
                        None,
                    )
                    .expect("create pumping test window")
                };
                raw_hwnds.push(hwnd);
                hwnds.push(hwnd.0 as isize);
            }
            let _ = tx.send((tid, hwnds));

            let mut msg = MSG::default();
            unsafe {
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                for h in raw_hwnds {
                    let _ = DestroyWindow(h);
                }
            }
        });

        let (thread_id, hwnds) = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("UI thread ready");
        Self {
            hwnds,
            thread_id,
            join: Some(join),
        }
    }
}

impl Drop for PumpingTestWindows {
    fn drop(&mut self) {
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn e2e_test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn find_own_windows_by_class(target_class: &str) -> Vec<HWND> {
    struct Search {
        pid: u32,
        target: String,
        found: Vec<HWND>,
    }

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let state = unsafe { &mut *(lparam.0 as *mut Search) };
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid != state.pid {
            return BOOL(1);
        }
        let mut buf = [0u16; 128];
        let len = unsafe { GetClassNameW(hwnd, &mut buf) };
        if len > 0 {
            let cls = String::from_utf16_lossy(&buf[..len as usize]);
            if cls == state.target {
                state.found.push(hwnd);
            }
        }
        BOOL(1)
    }

    let mut search = Search {
        pid: unsafe { GetCurrentProcessId() },
        target: target_class.to_string(),
        found: Vec::new(),
    };
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut search as *mut _ as isize));
    }
    search.found
}

#[test]
fn daemon_message_pump_handles_ipc_stash_toggle_cycle_and_shutdown() {
    let _guard = e2e_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("edgestash-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let pipe_name = format!(r"\\.\pipe\EdgeStashE2E-{}", std::process::id());
    let opts = DaemonLaunchOptions {
        pipe_name: pipe_name.clone(),
        config_path: dir.join("config.toml"),
        manifest_path: dir.join("active_stashes.json"),
        single_instance_mutex: None,
        enable_tray: false,
        enable_hotkeys: false,
        enable_win_events: false,
        enable_edge_triggers: false,
    };

    let (ready_tx, ready_rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        run_daemon_with_options(opts, Some(ready_tx)).expect("daemon runs cleanly");
    });

    ready_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("daemon signaled ready");

    let target_app = PumpingTestWindows::spawn(&["E2E Window 1", "E2E Window 2"]);
    let w1 = target_app.hwnds[0];
    let w2 = target_app.hwnds[1];

    let client = PipeClient::new(&pipe_name);
    let pong = client.send(&IpcRequest::Ping).expect("ping daemon");
    assert!(matches!(pong, IpcResponse::Pong { .. }));

    let r1 = client
        .send(&IpcRequest::StashWindow {
            hwnd: w1,
            edge: ScreenEdge::Right,
            group_with_active: false,
        })
        .expect("stash w1");
    assert!(matches!(r1, IpcResponse::Ok { .. }), "unexpected: {r1:?}");

    let r2 = client
        .send(&IpcRequest::StashWindow {
            hwnd: w2,
            edge: ScreenEdge::Right,
            group_with_active: false,
        })
        .expect("stash w2");
    assert!(matches!(r2, IpcResponse::Ok { .. }), "unexpected: {r2:?}");

    let st = client.send(&IpcRequest::Status).expect("status");
    match st {
        IpcResponse::Status(snap) => assert_eq!(snap.slots.len(), 2),
        other => panic!("unexpected status response: {other:?}"),
    }

    let cyc = client
        .send(&IpcRequest::CycleEdge {
            monitor_id: None,
            edge: ScreenEdge::Right,
            delta: 1,
        })
        .expect("cycle");
    assert!(matches!(cyc, IpcResponse::Ok { .. }));

    let shut = client.send(&IpcRequest::Shutdown).expect("shutdown");
    assert!(matches!(shut, IpcResponse::Ok { .. }));

    handle.join().expect("daemon exited cleanly");
    drop(target_app);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn daemon_shows_edge_pill_opens_on_pill_hover_and_exits_cleanly_on_non_ipc_shutdown() {
    let _guard = e2e_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("edgestash-pill-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cfg_path = dir.join("config.toml");
    std::fs::write(
        &cfg_path,
        "gesture_mode = \"CornerSlide\"\nedge_dwell_ms = 0\nopen_duration_ms = 50\nclose_duration_ms = 50\nauto_focus_on_open = false\ndisable_in_fullscreen = false\nedge_pill_style = \"AppIconsStack\"\n",
    )
    .unwrap();

    let pipe_name = format!(r"\\.\pipe\EdgeStashPillE2E-{}", std::process::id());
    let opts = DaemonLaunchOptions {
        pipe_name: pipe_name.clone(),
        config_path: cfg_path,
        manifest_path: dir.join("active_stashes.json"),
        single_instance_mutex: None,
        enable_tray: false,
        enable_hotkeys: false,
        enable_win_events: false,
        enable_edge_triggers: true,
    };

    let (ready_tx, ready_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let res = run_daemon_with_options(opts, Some(ready_tx));
        let _ = done_tx.send(res);
    });

    ready_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("daemon signaled ready");

    let mut msg_hwnds = Vec::new();
    for _ in 0..25 {
        msg_hwnds = find_own_windows_by_class("EdgeStashMessageWindow")
            .into_iter()
            .filter(|&h| unsafe { GetWindowLongPtrW(h, GWLP_USERDATA) != 0 })
            .collect();
        if !msg_hwnds.is_empty() {
            break;
        }
        thread::sleep(Duration::from_millis(40));
    }
    assert!(
        !msg_hwnds.is_empty(),
        "expected at least 1 active daemon message window"
    );
    let msg_hwnd = *msg_hwnds.last().unwrap();

    let target_app = PumpingTestWindows::spawn(&["PillHoverTarget"]);
    let win = target_app.hwnds[0];
    let client = PipeClient::new(&pipe_name);

    let stash_resp = client
        .send(&IpcRequest::StashWindow {
            hwnd: win,
            edge: ScreenEdge::Right,
            group_with_active: false,
        })
        .expect("stash right");
    assert!(matches!(stash_resp, IpcResponse::Ok { .. }));

    // Visible pill overlay window MUST be created and shown when a window is stashed
    let pill_hwnds: Vec<HWND> = find_own_windows_by_class("EdgeStashD2DOverlay")
        .into_iter()
        .filter(|&h| unsafe { IsWindowVisible(h).as_bool() })
        .collect();
    assert!(
        !pill_hwnds.is_empty(),
        "expected visible EdgeStashD2DOverlay pill after stashing window"
    );

    // Check real Win32 hit-testing on the D2D/DirectComposition pill center point
    let mut pr = RECT::default();
    unsafe {
        GetWindowRect(pill_hwnds[0], &mut pr).expect("GetWindowRect on pill");
    }
    let hit = unsafe {
        WindowFromPoint(POINT {
            x: (pr.left + pr.right) / 2,
            y: (pr.top + pr.bottom) / 2,
        })
    };
    assert_eq!(
        hit, pill_hwnds[0],
        "expected DirectComposition pill window to hit-test mouse input instead of passing through"
    );

    // Hovering (WM_MOUSEMOVE) on the visible pill must open the right-edge drawer even in CornerSlide mode
    unsafe {
        let _ = PostMessageW(pill_hwnds[0], WM_MOUSEMOVE, WPARAM(0), LPARAM(0));
    }
    thread::sleep(Duration::from_millis(120));

    let st = client
        .send(&IpcRequest::Status)
        .expect("status after pill hover");
    match st {
        IpcResponse::Status(snap) => {
            assert_eq!(snap.slots.len(), 1);
            assert!(
                snap.slots[0].is_open,
                "expected right-edge drawer to open after hovering on its pill"
            );
        }
        other => panic!("unexpected status: {other:?}"),
    }
    let ex = unsafe { GetWindowLongPtrW(HWND(win as *mut _), GWL_EXSTYLE) };
    assert_ne!(
        ex & (WS_EX_TOPMOST.0 as isize),
        0,
        "opened FloatingOverlay drawer window must carry WS_EX_TOPMOST"
    );

    // Close drawer, switch config to EdgeHover, reload, and verify 1px right trigger strip hover opens it too
    let close_resp = client
        .send(&IpcRequest::ToggleEdge {
            monitor_id: None,
            edge: ScreenEdge::Right,
        })
        .expect("close drawer via toggle");
    assert!(matches!(close_resp, IpcResponse::Ok { .. }));

    std::fs::write(
        dir.join("config.toml"),
        "gesture_mode = \"EdgeHover\"\nedge_dwell_ms = 0\nopen_duration_ms = 50\nclose_duration_ms = 50\nauto_focus_on_open = false\ndisable_in_fullscreen = false\nedge_pill_style = \"AppIconsStack\"\n",
    )
    .unwrap();
    let reload_resp = client
        .send(&IpcRequest::ReloadConfig)
        .expect("reload EdgeHover config");
    assert!(matches!(reload_resp, IpcResponse::Ok { .. }));

    let mut right_strip = None;
    for _ in 0..25 {
        right_strip = find_own_windows_by_class("EdgeStashTriggerStrip")
            .into_iter()
            .find(|&h| {
                let mut r = RECT::default();
                let owner = unsafe { GetWindowLongPtrW(h, GWLP_USERDATA) };
                let ok =
                    unsafe { IsWindowVisible(h).as_bool() && GetWindowRect(h, &mut r).is_ok() };
                owner == msg_hwnd.0 as isize
                    && ok
                    && (r.right - r.left) >= 1
                    && (r.right - r.left) <= 5
                    && (r.bottom - r.top) > 1
                    && r.left > 0
            });
        if right_strip.is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(40));
    }
    let right_strip = right_strip.expect("expected vertical Right trigger strip window");
    let mut opened = false;
    for _ in 0..30 {
        unsafe {
            let _ = PostMessageW(right_strip, WM_MOUSEMOVE, WPARAM(0), LPARAM(0));
        }
        thread::sleep(Duration::from_millis(40));
        if let Ok(IpcResponse::Status(snap)) = client.send(&IpcRequest::Status)
            && snap.slots.first().is_some_and(|s| s.is_open)
        {
            opened = true;
            break;
        }
    }
    assert!(
        opened,
        "expected right-edge drawer to open after hovering on 1px trigger strip in EdgeHover mode"
    );

    // Trigger non-IPC shutdown (simulating Tray Exit / Console Ctrl -> WM_APP_IPC wparam=0, lparam=0)
    // and ensure daemon exits promptly without hanging on ConnectNamedPipe!
    unsafe {
        let _ = PostMessageW(msg_hwnd, WM_APP + 1, WPARAM(0), LPARAM(0));
    }
    let exit_res = done_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("daemon must exit promptly on non-IPC shutdown instead of hanging in accept_one");
    exit_res.expect("daemon exited cleanly");
    handle.join().expect("thread joined");

    drop(target_app);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn daemon_area_aware_strip_wheel_header_pin_close_and_app_icons_click_e2e() {
    let _guard = e2e_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("edgestash-features-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cfg_path = dir.join("config.toml");
    std::fs::write(
        &cfg_path,
        concat!(
            "gesture_mode = \"EdgeHover\"\n",
            "edge_dwell_ms = 0\n",
            "open_duration_ms = 20\n",
            "close_duration_ms = 20\n",
            "dim_fade_duration_ms = 0\n",
            "auto_focus_on_open = false\n",
            "disable_in_fullscreen = false\n",
            "edge_pill_style = \"AppIconsStack\"\n",
            "area_aware_edge_scroll = true\n",
            "area_aware_edge_scroll_mode = \"AlwaysOnTriggerLine\"\n",
        ),
    )
    .unwrap();

    let pipe_name = format!(r"\\.\pipe\EdgeStashFeaturesE2E-{}", std::process::id());
    let opts = DaemonLaunchOptions {
        pipe_name: pipe_name.clone(),
        config_path: cfg_path,
        manifest_path: dir.join("active_stashes.json"),
        single_instance_mutex: None,
        enable_tray: false,
        enable_hotkeys: false,
        enable_win_events: false,
        enable_edge_triggers: true,
    };

    let (ready_tx, ready_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let res = run_daemon_with_options(opts, Some(ready_tx));
        let _ = done_tx.send(res);
    });

    ready_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("daemon signaled ready");
    let msg_hwnd = find_own_windows_by_class("EdgeStashMessageWindow")[0];

    let target_apps = PumpingTestWindows::spawn(&["AlphaApp", "BetaApp"]);
    let client = PipeClient::new(&pipe_name);
    for &hwnd in &target_apps.hwnds {
        let resp = client
            .send(&IpcRequest::StashWindow {
                hwnd,
                edge: ScreenEdge::Right,
                group_with_active: false,
            })
            .expect("stash test window");
        assert!(matches!(resp, IpcResponse::Ok { .. }));
    }

    // Wait briefly so pill layout & D2D surfaces finish updating.
    thread::sleep(Duration::from_millis(60));
    let pill_hwnd = find_own_windows_by_class("EdgeStashD2DOverlay")
        .into_iter()
        .find(|&h| {
            if !unsafe { IsWindowVisible(h).as_bool() } {
                return false;
            }
            let mut r = RECT::default();
            let ok = unsafe { GetWindowRect(h, &mut r).is_ok() };
            ok && (r.right - r.left) <= 72 && (r.bottom - r.top) >= 48
        })
        .expect("expected taller AppIconsStack pill for 2 stashed windows");

    // Area-aware wheel scroll anywhere along closed 1px Right trigger strip cycles and opens drawer!
    let mut right_strip_opt = None;
    for _ in 0..15 {
        right_strip_opt = find_own_windows_by_class("EdgeStashTriggerStrip")
            .into_iter()
            .find(|&h| {
                let mut r = RECT::default();
                let ok = unsafe { GetWindowRect(h, &mut r).is_ok() };
                ok && (r.right - r.left) >= 1
                    && (r.right - r.left) <= 5
                    && (r.bottom - r.top) > 1
                    && r.left > 0
            });
        if right_strip_opt.is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(40));
    }
    let right_strip = right_strip_opt.expect("expected vertical Right trigger strip");
    let mut sr = RECT::default();
    unsafe {
        GetWindowRect(right_strip, &mut sr).expect("GetWindowRect strip");
    }
    let strip_x = sr.left;
    let strip_y = sr.top + 40;
    let wheel_wparam = WPARAM(((-120i16) as u16 as usize) << 16);
    let wheel_lparam = LPARAM(((strip_x & 0xFFFF) | ((strip_y & 0xFFFF) << 16)) as isize);
    unsafe {
        let _ = PostMessageW(right_strip, WM_MOUSEWHEEL, wheel_wparam, wheel_lparam);
    }
    thread::sleep(Duration::from_millis(100));

    let st_open = match client
        .send(&IpcRequest::Status)
        .expect("status after strip wheel")
    {
        IpcResponse::Status(s) => s,
        other => panic!("unexpected status: {other:?}"),
    };
    assert_eq!(st_open.slots.len(), 2);
    assert!(
        st_open.slots.iter().any(|s| s.is_open),
        "expected strip WM_MOUSEWHEEL to open drawer in area-aware mode"
    );

    // Find the visible 28px-tall (DPI-scaled) header bar above the opened drawer and click Pin!
    let header_hwnd = find_own_windows_by_class("EdgeStashD2DOverlay")
        .into_iter()
        .find(|&h| {
            if !unsafe { IsWindowVisible(h).as_bool() } {
                return false;
            }
            let mut r = RECT::default();
            let ok = unsafe { GetWindowRect(h, &mut r).is_ok() };
            ok && (r.right - r.left) >= 200 && (r.bottom - r.top) <= 64
        })
        .expect("expected visible drawer header bar overlay");
    let mut hr = RECT::default();
    unsafe {
        GetWindowRect(header_hwnd, &mut hr).expect("GetWindowRect header");
    }
    let btns = header_button_rects(hr.right - hr.left, hr.bottom - hr.top);
    let pin_rect = btns
        .iter()
        .find(|(k, _)| *k == HeaderButtonKind::Pin)
        .map(|(_, r)| *r)
        .unwrap();
    let pin_lparam = LPARAM(
        (((pin_rect.left + pin_rect.width() / 2) & 0xFFFF)
            | (((pin_rect.top + pin_rect.height() / 2) & 0xFFFF) << 16)) as isize,
    );
    unsafe {
        let _ = PostMessageW(header_hwnd, WM_LBUTTONUP, WPARAM(0), pin_lparam);
    }
    thread::sleep(Duration::from_millis(80));

    let st_pinned = match client
        .send(&IpcRequest::Status)
        .expect("status after header Pin click")
    {
        IpcResponse::Status(s) => s,
        other => panic!("unexpected status: {other:?}"),
    };
    assert!(
        st_pinned
            .slots
            .iter()
            .any(|s| s.is_active_in_deck && s.dock_mode == DockMode::FloatingPinned),
        "expected header Pin click to switch active slot to FloatingPinned: {:?}",
        st_pinned.slots
    );

    // Click Close button on header -> drawer collapses cleanly back to edge!
    let close_rect = btns
        .iter()
        .find(|(k, _)| *k == HeaderButtonKind::Close)
        .map(|(_, r)| *r)
        .unwrap();
    let close_lparam = LPARAM(
        (((close_rect.left + close_rect.width() / 2) & 0xFFFF)
            | (((close_rect.top + close_rect.height() / 2) & 0xFFFF) << 16)) as isize,
    );
    unsafe {
        let _ = PostMessageW(header_hwnd, WM_LBUTTONUP, WPARAM(0), close_lparam);
    }
    thread::sleep(Duration::from_millis(80));

    let st_closed = match client
        .send(&IpcRequest::Status)
        .expect("status after header Close click")
    {
        IpcResponse::Status(s) => s,
        other => panic!("unexpected status: {other:?}"),
    };
    assert!(
        st_closed.slots.iter().all(|s| !s.is_open),
        "expected header Close click to collapse drawer: {:?}",
        st_closed.slots
    );

    // Click bottom half of the AppIconsStack pill (slot 1 -> "BetaApp") to open slot 1 directly!
    let mut pr = RECT::default();
    unsafe {
        GetWindowRect(pill_hwnd, &mut pr).expect("GetWindowRect pill");
    }
    let pw = pr.right - pr.left;
    let ph = pr.bottom - pr.top;
    let click_slot1 = LPARAM((((pw / 2) & 0xFFFF) | (((ph * 3 / 4) & 0xFFFF) << 16)) as isize);
    unsafe {
        let _ = PostMessageW(pill_hwnd, WM_LBUTTONUP, WPARAM(0), click_slot1);
    }
    thread::sleep(Duration::from_millis(80));

    let st_slot1 = match client
        .send(&IpcRequest::Status)
        .expect("status after AppIconsStack icon click")
    {
        IpcResponse::Status(s) => s,
        other => panic!("unexpected status: {other:?}"),
    };
    let active = st_slot1
        .slots
        .iter()
        .find(|s| s.is_active_in_deck)
        .expect("active slot");
    assert!(active.is_open, "expected icon click to open clicked slot");
    assert_eq!(
        active.slot_index, 1,
        "expected bottom icon click to open 2nd slot"
    );

    unsafe {
        let _ = PostMessageW(msg_hwnd, WM_APP + 1, WPARAM(0), LPARAM(0));
    }
    done_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("daemon exited cleanly")
        .expect("daemon ok");
    handle.join().expect("thread joined");

    drop(target_apps);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn daemon_handles_taskbar_created_broadcast_when_explorer_restarts() {
    let _guard = e2e_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("edgestash_taskbar_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cfg_path = dir.join("config.toml");
    EdgeStashConfig::default().save_to_at(&cfg_path).unwrap();

    let pipe_name = format!(r"\\.\pipe\EdgeStashTaskbarTest-{}", std::process::id());
    let opts = DaemonLaunchOptions {
        pipe_name: pipe_name.clone(),
        config_path: cfg_path,
        manifest_path: dir.join("active_stashes.json"),
        single_instance_mutex: None,
        enable_tray: true,
        enable_hotkeys: false,
        enable_win_events: false,
        enable_edge_triggers: false,
    };

    let (ready_tx, ready_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let res = run_daemon_with_options(opts, Some(ready_tx));
        let _ = done_tx.send(res);
    });

    ready_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("daemon signaled ready");
    let msg_hwnd = find_own_windows_by_class("EdgeStashMessageWindow")[0];

    // Simulate Explorer restarting by broadcasting "TaskbarCreated" registered message
    let taskbar_msg = unsafe {
        use windows::Win32::UI::WindowsAndMessaging::RegisterWindowMessageW;
        use windows::core::w;
        RegisterWindowMessageW(w!("TaskbarCreated"))
    };
    assert_ne!(taskbar_msg, 0);

    // Send TaskbarCreated to daemon message window
    unsafe {
        let _ = PostMessageW(msg_hwnd, taskbar_msg, WPARAM(0), LPARAM(0));
    }
    thread::sleep(Duration::from_millis(60));

    // Daemon processes the message cleanly without crashing and remains responsive over IPC
    let client = PipeClient::new(&pipe_name);
    let ping_resp = client.send(&IpcRequest::Ping).expect("ping daemon");
    assert!(matches!(ping_resp, IpcResponse::Pong { .. }));

    // Clean shutdown
    let _ = client.send(&IpcRequest::Shutdown);
    let _ = done_rx.recv_timeout(Duration::from_secs(3));
    let _ = handle.join();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn resolve_settings_ui_binary_uses_sibling_exe_or_path_fallback() {
    use std::path::{Path, PathBuf};

    use edgestash_daemon::daemon::resolve_settings_ui_binary;

    assert_eq!(
        resolve_settings_ui_binary(Some(Path::new(r"C:\Apps\EdgeStash\edgestash-daemon.exe"))),
        PathBuf::from(r"C:\Apps\EdgeStash\edgestash-ui.exe")
    );
    assert_eq!(
        resolve_settings_ui_binary(None),
        PathBuf::from("edgestash-ui.exe")
    );
}

#[test]
#[ignore = "Modifies live desktop work area via SHAppBarMessage; run explicitly with --ignored"]
fn live_appbar_registration_and_removal_smoke_test() {
    use edgestash_daemon::appbar::register_or_update_native_dock;
    use edgestash_ipc::types::Rect;

    let app = PumpingTestWindows::spawn(&["EdgeStashAppBarTest"]);
    let helper = HWND(app.hwnds[0] as *mut _);
    register_or_update_native_dock(
        helper,
        ScreenEdge::Right,
        Rect::new(0, 0, 1920, 1080),
        320,
        true,
    );
    register_or_update_native_dock(
        helper,
        ScreenEdge::Right,
        Rect::new(0, 0, 1920, 1080),
        320,
        false,
    );
}
