use std::mem::size_of;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Instant;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DT_CENTER, DT_VCENTER, DT_WORDBREAK, DeleteObject, DrawTextW,
    EndPaint, FillRect, InvalidateRect, PAINTSTRUCT, SetBkMode, SetTextColor, TRANSPARENT,
    UpdateWindow,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, FLASHW_ALL, FLASHW_TIMERNOFG, FLASHWINFO,
    FlashWindowEx, GetClientRect, GetMessageW, IDC_ARROW, IDI_APPLICATION, KillTimer, LoadCursorW,
    LoadIconW, MSG, PostQuitMessage, RegisterClassW, SW_SHOW, SetTimer, ShowWindow,
    TranslateMessage, WM_CREATE, WM_DESTROY, WM_KEYDOWN, WM_LBUTTONDOWN, WM_PAINT, WM_TIMER,
    WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};
use windows::core::w;

static COUNTDOWN_SECS: AtomicI32 = AtomicI32::new(30);
static LAST_FLASH_DESC: Mutex<Option<String>> = Mutex::new(None);

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            // Set 1-second countdown timer
            let _ = unsafe { SetTimer(hwnd, 1, 1000, None) };
            println!("============================================================");
            println!("  EdgeStash Alert Tester Started!");
            println!("============================================================");
            println!("Instructions:");
            println!("1. Stash this window at any edge (e.g. Win+Right, or bump).");
            println!("2. Wait for the 30-second countdown (or press SPACE / click to flash now).");
            println!("3. Watch the EdgeStash pill at that edge: it will pulse amber and");
            println!("   show the notification dot, simulating an incoming chat message!");
            println!("4. Hover or open the drawer: the notification clears automatically.");
            println!("============================================================\n");
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == 1 {
                let current = COUNTDOWN_SECS.fetch_sub(1, Ordering::SeqCst) - 1;
                if current <= 0 {
                    trigger_flash(hwnd, "Timer (30s interval)");
                    COUNTDOWN_SECS.store(30, Ordering::SeqCst);
                }
                let _ = unsafe { InvalidateRect(hwnd, None, false) };
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            // 0x20 is VK_SPACE, 0x46 is VK_F
            if wparam.0 == 0x20 || wparam.0 == 0x46 {
                trigger_flash(hwnd, "Manual Trigger (Space/F key)");
                COUNTDOWN_SECS.store(30, Ordering::SeqCst);
                let _ = unsafe { InvalidateRect(hwnd, None, false) };
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            trigger_flash(hwnd, "Mouse Click");
            COUNTDOWN_SECS.store(30, Ordering::SeqCst);
            let _ = unsafe { InvalidateRect(hwnd, None, false) };
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
            let mut client = RECT::default();
            let _ = unsafe { GetClientRect(hwnd, &mut client) };

            // Draw dark sleek background
            let bg_brush = unsafe { CreateSolidBrush(COLORREF(0x001E1E1E)) };
            let _ = unsafe { FillRect(hdc, &client, bg_brush) };
            let _ = unsafe { DeleteObject(bg_brush) };

            unsafe { SetBkMode(hdc, TRANSPARENT) };

            let desc_guard = LAST_FLASH_DESC.lock().unwrap();
            let status_text = format!(
                "EdgeStash Window Alert Simulator\n\n\
                Next simulated notification in: {} seconds\n\n\
                [Click anywhere or press SPACE to flash immediately]\n\n\
                Status: {}\n\n\
                Stash this window at an edge and watch the pill pulse amber!",
                COUNTDOWN_SECS.load(Ordering::SeqCst),
                desc_guard.as_deref().unwrap_or("Waiting to flash...")
            );
            drop(desc_guard);

            let mut wide: Vec<u16> = status_text.encode_utf16().collect();
            unsafe {
                SetTextColor(hdc, COLORREF(0x0000D0FF)); // Gold / Amber accent text
                DrawTextW(
                    hdc,
                    &mut wide,
                    &mut client,
                    DT_CENTER | DT_VCENTER | DT_WORDBREAK,
                );
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe {
                let _ = KillTimer(hwnd, 1);
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn trigger_flash(hwnd: HWND, reason: &str) {
    let fwi = FLASHWINFO {
        cbSize: size_of::<FLASHWINFO>() as u32,
        hwnd,
        dwFlags: FLASHW_ALL | FLASHW_TIMERNOFG,
        uCount: 10,
        dwTimeout: 0,
    };
    unsafe {
        let _ = FlashWindowEx(&fwi);
    }
    let desc = format!("Flashed at {:?} ({})", Instant::now(), reason);
    println!("[NOTIFICATION TRIGGERED] {desc}");
    if let Ok(mut lock) = LAST_FLASH_DESC.lock() {
        *lock = Some(desc);
    }
}

fn main() {
    let result = run_app();
    if let Err(e) = result {
        eprintln!("[FATAL ERROR] {e:?}");
        eprintln!("Press Enter to exit...");
        let mut s = String::new();
        let _ = std::io::stdin().read_line(&mut s);
    }
}

fn run_app() -> windows::core::Result<()> {
    unsafe {
        let hinst = GetModuleHandleW(None)?;
        let cls_name = w!("ChatAlertSimulatorWindowClass");

        let wc = WNDCLASSW {
            hInstance: hinst.into(),
            lpszClassName: cls_name,
            lpfnWndProc: Some(wnd_proc),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hIcon: LoadIconW(None, IDI_APPLICATION).unwrap_or_default(),
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            cls_name,
            w!("EdgeStash Alert Tester (Messenger/Slack Simulator)"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            200,
            200,
            500,
            360,
            None,
            None,
            hinst,
            None,
        )?;

        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        Ok(())
    }
}
