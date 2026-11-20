use edgestash_daemon::dwm::{get_invisible_border_insets, is_window_cloaked, set_window_cloaked};
use edgestash_daemon::geometry::HeaderButtonKind;
use edgestash_daemon::platform::win32::{Win32WindowSystem, Win32VirtualDesktop};
use edgestash_daemon::platform::{VirtualDesktopApi, WindowHandle, WindowSystem};
use edgestash_daemon::renderer::d2d::{D2DGraphicsContext, DrawerHeaderVisual, PillSlotVisual};
use edgestash_ipc::types::{DockMode, EdgePillStyle};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_OVERLAPPEDWINDOW,
    WS_POPUP,
};
use windows::core::w;

struct TestHwnd(HWND);

impl TestHwnd {
    fn create_hidden_overlapped() -> Self {
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("STATIC"),
                w!("EdgeStashTestWindow"),
                WS_OVERLAPPEDWINDOW,
                -32000,
                -32000,
                400,
                300,
                None,
                None,
                None,
                None,
            )
            .expect("create test window")
        };
        Self(hwnd)
    }

    fn create_popup() -> Self {
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("STATIC"),
                w!("EdgeStashD2DTest"),
                WS_POPUP,
                -32000,
                -32000,
                64,
                64,
                None,
                None,
                None,
                None,
            )
            .expect("create d2d test window")
        };
        Self(hwnd)
    }
}

impl Drop for TestHwnd {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}

#[test]
fn real_win32_dwm_cloak_and_border_insets_work() {
    let win = TestHwnd::create_hidden_overlapped();
    let sys = Win32WindowSystem::new();

    // Cloaking toggles DWMWA_CLOAKED accurately on real HWND
    set_window_cloaked(win.0, true).expect("cloak window");
    assert!(is_window_cloaked(win.0));
    set_window_cloaked(win.0, false).expect("uncloak window");
    assert!(!is_window_cloaked(win.0));

    // Snapshot & monitor enumeration succeed
    let monitors = sys.enumerate_monitors();
    assert!(!monitors.is_empty());
    assert!(monitors.iter().any(|m| m.is_primary));
    assert!(monitors[0].rect.width() > 0 && monitors[0].dpi >= 96);

    let snap = sys
        .snapshot_window(WindowHandle::from_hwnd(win.0))
        .expect("snapshot test window");
    assert_eq!(snap.title, "EdgeStashTestWindow");
    let _ = get_invisible_border_insets(win.0);

    // Problem 2: Stashing window MUST NOT apply WS_EX_TOOLWINDOW (which strips Minimize/Maximize buttons)
    let app_win = TestHwnd(unsafe {
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WS_EX_APPWINDOW,
            w!("STATIC"),
            w!("ExplorerAppWindow"),
            WS_OVERLAPPEDWINDOW,
            -32000,
            -32000,
            400,
            300,
            None,
            None,
            None,
            None,
        )
        .expect("create app test window")
    });
    let orig_ex = unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(app_win.0, windows::Win32::UI::WindowsAndMessaging::GWL_EXSTYLE) };
    assert_eq!(orig_ex & (windows::Win32::UI::WindowsAndMessaging::WS_EX_TOOLWINDOW.0 as isize), 0);
    sys.prepare_window_for_stash(WindowHandle::from_hwnd(app_win.0), orig_ex)
        .expect("prepare_window_for_stash succeeds");
    let stashed_ex = unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(app_win.0, windows::Win32::UI::WindowsAndMessaging::GWL_EXSTYLE) };
    assert_eq!(
        stashed_ex & (windows::Win32::UI::WindowsAndMessaging::WS_EX_TOOLWINDOW.0 as isize),
        0,
        "prepare_window_for_stash must not add WS_EX_TOOLWINDOW so native titlebar buttons remain normal"
    );
}

struct ComGuard;
impl ComGuard {
    fn init() -> Self {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        Self
    }
}
impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

#[test]
fn real_com_virtual_desktop_manager_initializes_and_queries() {
    let _com = ComGuard::init();
    let helper = TestHwnd::create_hidden_overlapped();
    let vdm = Win32VirtualDesktop::new(helper.0).expect("IVirtualDesktopManager initializes");
    let _ = vdm.is_on_current_desktop(WindowHandle::from_hwnd(helper.0));
    let _ = vdm.get_current_desktop_id();
}

#[test]
fn real_direct2d_and_directcomposition_pipeline_renders_offscreen() {
    let ctx = D2DGraphicsContext::new().expect("Direct2D + DirectComposition device initializes");
    let popup = TestHwnd::create_popup();
    let mut surface = ctx
        .create_window_surface(popup.0, 48, 120)
        .expect("create DirectComposition swapchain surface");
    let slots = [
        PillSlotVisual {
            icon: None,
            title: "Code",
            needs_attention: false,
        },
        PillSlotVisual {
            icon: None,
            title: "Terminal",
            needs_attention: true,
        },
        PillSlotVisual {
            icon: None,
            title: "Notes",
            needs_attention: false,
        },
    ];
    surface
        .render_pill(48, 120, EdgePillStyle::AppIconsStack, &slots, 3, 1, None, 1, 1000)
        .expect("Direct2D AppIconsStack pill render + Present + DComp Commit succeeds");
    surface
        .render_pill(20, 96, EdgePillStyle::MinimalLine, &slots, 3, 1, None, 1, 1000)
        .expect("Direct2D MinimalLine pill render succeeds");
    surface
        .render_drawer_header(
            360,
            28,
            &DrawerHeaderVisual {
                title: "Notes",
                slot_index: 1,
                slot_count: 3,
                dock_mode: DockMode::FloatingPinned,
                hovered_button: Some(HeaderButtonKind::Pin),
            },
        )
        .expect("Direct2D drawer header render succeeds");
    surface
        .render_scrim_alpha(64, 64, 0.25)
        .expect("Direct2D alpha scrim render succeeds");
}

#[test]
fn test_bring_drag_image_to_top_promotes_drag_preview_above_topmost_drawer() {
    use edgestash_daemon::platform::win32::bring_drag_image_to_top;
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, GW_HWNDNEXT, GetTopWindow, GetWindow,
        HWND_TOPMOST, RegisterClassW, SetWindowPos, ShowWindow, SWP_NOACTIVATE, SWP_NOMOVE,
        SWP_NOSIZE, SW_SHOWNA, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOPMOST,
        WS_EX_TRANSPARENT, WS_POPUP,
    };
    use windows::core::w;

    unsafe extern "system" fn test_drag_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    let hinst = unsafe { GetModuleHandleW(None).unwrap() };
    let wc = WNDCLASSW {
        hInstance: hinst.into(),
        lpszClassName: w!("SysDragImage"),
        lpfnWndProc: Some(test_drag_wnd_proc),
        ..Default::default()
    };
    unsafe {
        let _ = RegisterClassW(&wc);
    }

    // 1. Create a simulated SysDragImage window
    let drag_hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE,
            w!("SysDragImage"),
            w!("SysDragImage"),
            WS_POPUP,
            100,
            100,
            50,
            50,
            None,
            None,
            hinst,
            None,
        )
        .expect("create SysDragImage")
    };
    unsafe {
        let _ = ShowWindow(drag_hwnd, SW_SHOWNA);
    }

    // 2. Create a simulated stashed drawer window and promote it to HWND_TOPMOST
    let drawer_hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            w!("STATIC"),
            w!("EdgeStashDrawerTest"),
            WS_POPUP,
            50,
            50,
            400,
            400,
            None,
            None,
            hinst,
            None,
        )
        .expect("create drawer window")
    };
    unsafe {
        let _ = ShowWindow(drawer_hwnd, SW_SHOWNA);
        let _ = SetWindowPos(
            drawer_hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }

    // 3. Call bring_drag_image_to_top()
    bring_drag_image_to_top();

    // Verify relative Z-order: drag_hwnd must be placed ahead of drawer_hwnd in the topmost band
    let mut z_order = Vec::new();
    let mut curr = unsafe { GetTopWindow(HWND::default()).ok() };
    while let Some(w) = curr {
        if w.0.is_null() {
            break;
        }
        if w == drag_hwnd || w == drawer_hwnd {
            z_order.push(w);
        }
        curr = unsafe { GetWindow(w, GW_HWNDNEXT).ok() };
    }
    assert_eq!(z_order, vec![drag_hwnd, drawer_hwnd]);

    // Cleanup
    unsafe {
        let _ = DestroyWindow(drawer_hwnd);
        let _ = DestroyWindow(drag_hwnd);
    }
}
