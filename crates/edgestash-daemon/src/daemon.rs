use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::thread;

use edgestash_ipc::config::{DragDropModifierKey, EdgeStashConfig, GestureMode, HotkeySpec};
use edgestash_ipc::pipe::{PipeClient, PipeServer};
use edgestash_ipc::protocol::{DEFAULT_PIPE_NAME, IpcErrorCode, IpcRequest, IpcResponse};
use edgestash_ipc::types::{ChurnZoneId, DockMode, EdgePillStyle, Rect, ScreenEdge};
use windows::Win32::Foundation::{
    BOOL, CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HINSTANCE, HWND, LPARAM,
    LRESULT, POINT, POINTL, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{COLOR_WINDOW, ClientToScreen, HBRUSH};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_FILE_NAME,
    FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING, ReadDirectoryChangesW,
};
use windows::Win32::System::Com::IDataObject;
use windows::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    SetConsoleCtrlHandler,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{
    DROPEFFECT, DROPEFFECT_NONE, IDropTarget, IDropTarget_Impl, OleInitialize, OleUninitialize,
    RegisterDragDrop, RevokeDragDrop,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, HOT_KEY_MODIFIERS, RegisterHotKey, TME_LEAVE, TRACKMOUSEEVENT,
    TrackMouseEvent, UnregisterHotKey, VK_CONTROL, VK_LBUTTON, VK_LWIN, VK_MENU, VK_RBUTTON,
    VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CHILDID_SELF, CallNextHookEx, ChangeWindowMessageFilterEx, CreatePopupMenu,
    CreateWindowExW, DefWindowProcW, DeregisterShellHookWindow, DestroyMenu, DestroyWindow,
    DispatchMessageW, EVENT_OBJECT_DESTROY, EVENT_OBJECT_LOCATIONCHANGE, EVENT_SYSTEM_FOREGROUND,
    EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MOVESIZEEND,
    EVENT_SYSTEM_MOVESIZESTART, FindWindowW, GA_ROOT, GWL_EXSTYLE, GWLP_USERDATA, GetAncestor,
    GetCursorPos, GetMessageW, GetWindowLongPtrW, GetWindowRect, HHOOK, HWND_NOTOPMOST,
    HWND_TOPMOST, IDC_ARROW, IDI_APPLICATION, IsZoomed, KillTimer, LoadCursorW, LoadIconW,
    MF_CHECKED, MF_SEPARATOR, MF_STRING, MSG, MSGFLT_ALLOW, MSLLHOOKSTRUCT, OBJID_WINDOW,
    PM_NOREMOVE, PeekMessageW, PostMessageW, PostQuitMessage, RegisterClassW,
    RegisterShellHookWindow, RegisterWindowMessageW, SW_HIDE, SW_RESTORE, SW_SHOWNOACTIVATE,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetForegroundWindow,
    SetTimer, SetWindowLongPtrW, SetWindowPos, SetWindowsHookExW, ShowWindow, TPM_BOTTOMALIGN,
    TPM_LEFTALIGN, TPM_RETURNCMD, TrackPopupMenu, TranslateMessage, UnhookWindowsHookEx,
    WH_MOUSE_LL, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_APP, WM_DESTROY,
    WM_DISPLAYCHANGE, WM_ENDSESSION, WM_HOTKEY, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCREATE, WM_NCLBUTTONDOWN, WM_QUERYENDSESSION, WM_RBUTTONUP,
    WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP, WindowFromPoint,
};
use windows::core::{PCWSTR, Result as WinResult, implement, w};

use crate::crash_recovery::{CrashManifest, recover_orphans_with};
use crate::geometry::{HeaderButtonKind, header_button_rects};
use crate::platform::win32::{
    Win32AppBar, Win32VirtualDesktop, Win32WindowSystem, bring_drag_image_to_top,
};
use crate::platform::{
    Clock, MonitorInfo, PillOverlayEvent, SystemClock, UnstashTargetState, VisualOverlayApi,
    WindowHandle, WindowSystem,
};
use crate::renderer::DimScrimFade;
use crate::renderer::d2d::{
    CloseAnimFrame, D2DGraphicsContext, D2DWindowSurface, DrawerHeaderVisual, IconPixels,
    PillSlotVisual, extract_window_icon_bgra,
};
use crate::state::EdgeStashEngine;
use crate::trigger::{
    AreaAwareScrollController, DirectedCharmsDetector, DragCaptureStateMachine, DragDropTarget,
    EdgeBumpStateMachine, EdgeHoverStateMachine, StripScrollDecision, allows_pill_click,
    allows_pill_hover, allows_strip_hover, area_scroll_shade_rect, compute_churn_preview_rect,
    compute_dim_scrim_insert_after, compute_edge_band_rect_inset, detect_near_edge_ext,
    edge_pill_rect_for_style, hit_test_pill_slot_index, is_in_area_scroll_line,
    is_strip_input_transparent, should_pill_be_visible, trigger_strip_rect,
};

const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_APP_IPC: u32 = WM_APP + 1;
const WM_APP_RELOAD_CONFIG: u32 = WM_APP + 2;
const WM_APP_TRAY: u32 = WM_APP + 3;
const WM_APP_WIN_FOREGROUND: u32 = WM_APP + 4;
const WM_APP_WIN_MOVESIZESTART: u32 = WM_APP + 5;
const WM_APP_WIN_MOVESIZEEND: u32 = WM_APP + 6;
const WM_APP_WIN_DESTROY: u32 = WM_APP + 7;
const WM_APP_DRAG_ENTER: u32 = WM_APP + 8;
const WM_APP_DRAG_OVER: u32 = WM_APP + 9;
const WM_APP_DRAG_LEAVE: u32 = WM_APP + 10;
const WM_APP_DRAG_DROP: u32 = WM_APP + 11;
const WM_APP_DRAG_WHEEL: u32 = WM_APP + 12;
const WM_APP_WIN_LOCATIONCHANGE: u32 = WM_APP + 13;
const WM_APP_GLOBAL_MOUSEMOVE: u32 = WM_APP + 14;
const WM_APP_EDGE_WHEEL: u32 = WM_APP + 15;
const WM_APP_WIN_MINIMIZE: u32 = WM_APP + 16;

const TIMER_DISMISS_CHECK: usize = 1;
const TIMER_EDGE_HOVER: usize = 2;
const TIMER_DRAG_POLL: usize = 3;
const TIMER_CLOSE_ANIM: usize = 4;
const TIMER_CHARMS_POLL: usize = 5;
const TIMER_DIM_FADE: usize = 6;
const TIMER_ATTENTION_PULSE: usize = 7;
const TIMER_TRAY_RETRY: usize = 8;

const HOTKEY_STASH_LEFT: i32 = 1;
const HOTKEY_STASH_RIGHT: i32 = 2;
const HOTKEY_STASH_TOP: i32 = 3;
const HOTKEY_STASH_BOTTOM: i32 = 4;
const HOTKEY_TOGGLE_ACTIVE: i32 = 5;
const HOTKEY_GROUP_ACTIVE: i32 = 6;
const HOTKEY_UNSTASH_ACTIVE: i32 = 7;

const TRAY_CMD_UNSTASH_ALL: u32 = 1001;
const TRAY_CMD_RELOAD: u32 = 1002;
const TRAY_CMD_EXIT: u32 = 1003;
const TRAY_CMD_TOGGLE_GESTURE_MODE: u32 = 1004;
const TRAY_CMD_SETTINGS: u32 = 1005;

const PILL_CMD_TOGGLE: u32 = 2001;
const PILL_CMD_UNSTASH: u32 = 2002;
const PILL_CMD_TOGGLE_DOCK: u32 = 2003;
const PILL_CMD_SETTINGS: u32 = 2004;
const PILL_CMD_PIN_FLOATING: u32 = 2005;
const PILL_CMD_UNPIN: u32 = 2006;

const HEADER_CMD_PIN_FLOATING: u32 = 3001;
const HEADER_CMD_PIN_DOCK: u32 = 3002;
const HEADER_CMD_UNPIN: u32 = 3003;

/// Resolves the `edgestash-ui.exe` sibling binary beside `current_exe`,
/// falling back to `PATH` when no parent directory is available.
pub fn resolve_settings_ui_binary(current_exe: Option<&Path>) -> PathBuf {
    if let Some(dir) = current_exe.and_then(|p| p.parent())
        && !dir.as_os_str().is_empty()
    {
        return dir.join("edgestash-ui.exe");
    }
    PathBuf::from("edgestash-ui.exe")
}

/// Focuses an open `EdgeStash Settings` window if present, otherwise launches `edgestash-ui.exe`.
pub fn launch_settings_ui() {
    if let Ok(hwnd) = unsafe { FindWindowW(None, w!("EdgeStash Settings")) }
        && !hwnd.0.is_null()
    {
        unsafe {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
        }
        return;
    }

    let current_exe = std::env::current_exe().ok();
    let ui_exe = resolve_settings_ui_binary(current_exe.as_deref());
    let _ = std::process::Command::new(ui_exe).spawn();
}

fn is_drag_modifier_held(modifier: DragDropModifierKey) -> bool {
    match modifier {
        DragDropModifierKey::Shift => unsafe {
            (GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0
        },
        DragDropModifierKey::Ctrl => unsafe {
            (GetAsyncKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0
        },
        DragDropModifierKey::Alt => unsafe {
            (GetAsyncKeyState(VK_MENU.0 as i32) as u16 & 0x8000) != 0
        },
        DragDropModifierKey::Any => unsafe {
            (GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0
                || (GetAsyncKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0
                || (GetAsyncKeyState(VK_MENU.0 as i32) as u16 & 0x8000) != 0
                || (GetAsyncKeyState(VK_RBUTTON.0 as i32) as u16 & 0x8000) != 0
        },
    }
}

static EMERGENCY_MANIFEST_PATH: OnceLock<PathBuf> = OnceLock::new();
static SAFETY_HOOKS_INSTALLED: OnceLock<()> = OnceLock::new();
static GLOBAL_MSG_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static DRAG_WHEEL_HOOK: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static GLOBAL_MOUSE_HOOK: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static WM_SHELLHOOK_MSG: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static WM_TASKBAR_CREATED_MSG: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static LAST_MINIMIZE_CLICK_HWND: std::sync::atomic::AtomicIsize =
    std::sync::atomic::AtomicIsize::new(0);
static LAST_MINIMIZE_CLICK_TIME: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

#[derive(Default)]
struct LowLevelMouseSnapshot {
    monitors: Vec<MonitorInfo>,
    corner_zone_px: i32,
    should_track_bezel_moves: bool,
    active_charms_or_hover: bool,
    was_near_edge: bool,
    wheel_capture_zones: Vec<(String, ScreenEdge, Rect)>,
}

thread_local! {
    static OPEN_STASHED_HWNDS: RefCell<HashSet<isize>> = RefCell::new(HashSet::new());
    static LL_MOUSE_STATE: RefCell<LowLevelMouseSnapshot> = RefCell::new(LowLevelMouseSnapshot::default());
}

fn edge_to_code(edge: ScreenEdge) -> usize {
    match edge {
        ScreenEdge::Left => 0,
        ScreenEdge::Right => 1,
        ScreenEdge::Top => 2,
        ScreenEdge::Bottom => 3,
    }
}

fn code_to_edge(code: usize) -> Option<ScreenEdge> {
    match code {
        0 => Some(ScreenEdge::Left),
        1 => Some(ScreenEdge::Right),
        2 => Some(ScreenEdge::Top),
        3 => Some(ScreenEdge::Bottom),
        _ => None,
    }
}

fn point_near_any_monitor_edge(monitors: &[MonitorInfo], pt: POINT, margin_px: i32) -> bool {
    let m = margin_px.max(8);
    monitors.iter().any(|mon| {
        let r = mon.rect;
        pt.x >= r.left - 24
            && pt.x <= r.right + 24
            && pt.y >= r.top - 24
            && pt.y <= r.bottom + 24
            && (pt.x <= r.left + m
                || pt.x >= r.right - m
                || pt.y <= r.top + m
                || pt.y >= r.bottom - m)
    })
}

unsafe extern "system" fn global_mouse_hook_proc(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if code >= 0 && lparam.0 != 0 {
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        let msg_raw = GLOBAL_MSG_HWND.load(Ordering::Acquire);
        if msg_raw != 0 {
            let msg = wparam.0 as u32;
            if msg == WM_MOUSEMOVE {
                let should_post = LL_MOUSE_STATE.with(|cell| {
                    let mut st = cell.borrow_mut();
                    if !st.should_track_bezel_moves {
                        st.was_near_edge = false;
                        return false;
                    }
                    let margin = (st.corner_zone_px * 4).max(48);
                    let near = point_near_any_monitor_edge(&st.monitors, info.pt, margin);
                    let notify = near || st.active_charms_or_hover || st.was_near_edge;
                    st.was_near_edge = near;
                    notify
                });
                if should_post {
                    unsafe {
                        let _ = PostMessageW(
                            HWND(msg_raw as *mut _),
                            WM_APP_GLOBAL_MOUSEMOVE,
                            WPARAM(info.pt.x as isize as usize),
                            LPARAM(info.pt.y as isize),
                        );
                    }
                }
            } else if msg == WM_MOUSEWHEEL {
                let delta = ((info.mouseData >> 16) as u16) as i16;
                if delta != 0 {
                    let captured = LL_MOUSE_STATE.with(|cell| {
                        let st = cell.borrow();
                        st.wheel_capture_zones
                            .iter()
                            .enumerate()
                            .find(|(_, (_, _, rect))| rect.contains_inclusive(info.pt.x, info.pt.y))
                            .map(|(idx, (_, edge, _))| (idx, *edge))
                    });
                    if let Some((idx, edge)) = captured {
                        unsafe {
                            let _ = PostMessageW(
                                HWND(msg_raw as *mut _),
                                WM_APP_EDGE_WHEEL,
                                WPARAM((delta as u16) as usize),
                                LPARAM(((idx << 4) | edge_to_code(edge)) as isize),
                            );
                        }
                        return LRESULT(1);
                    }
                }
            } else if msg == WM_LBUTTONDOWN || msg == WM_NCLBUTTONDOWN {
                let hit_stashed = OPEN_STASHED_HWNDS.with(|cell| {
                    let set = cell.borrow();
                    if set.is_empty() {
                        return None;
                    }
                    unsafe {
                        let target = WindowFromPoint(info.pt);
                        if target.0.is_null() {
                            return None;
                        }
                        let root = GetAncestor(target, GA_ROOT);
                        if set.contains(&(root.0 as isize)) {
                            Some(root)
                        } else {
                            None
                        }
                    }
                });
                if let Some(root_hwnd) = hit_stashed {
                    unsafe {
                        let mut wr = RECT::default();
                        if GetWindowRect(root_hwnd, &mut wr).is_ok() {
                            let in_caption = info.pt.y >= wr.top - 8
                                && info.pt.y <= wr.top + 50
                                && info.pt.x >= wr.left
                                && info.pt.x <= wr.right;
                            if in_caption {
                                let now_ms = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_millis() as u64)
                                    .unwrap_or(0);
                                LAST_MINIMIZE_CLICK_HWND
                                    .store(root_hwnd.0 as isize, Ordering::Release);
                                LAST_MINIMIZE_CLICK_TIME.store(now_ms, Ordering::Release);
                            }
                        }
                    }
                }
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn install_global_mouse_hook() {
    if GLOBAL_MOUSE_HOOK.load(Ordering::Acquire) != 0 {
        return;
    }
    if let Ok(hook) =
        unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(global_mouse_hook_proc), None, 0) }
        && !hook.is_invalid()
    {
        let prev = GLOBAL_MOUSE_HOOK.swap(hook.0 as isize, Ordering::AcqRel);
        if prev != 0 {
            unsafe {
                let _ = UnhookWindowsHookEx(HHOOK(prev as *mut _));
            }
        }
    }
}

fn uninstall_global_mouse_hook() {
    let raw = GLOBAL_MOUSE_HOOK.swap(0, Ordering::AcqRel);
    if raw != 0 {
        unsafe {
            let _ = UnhookWindowsHookEx(HHOOK(raw as *mut _));
        }
    }
}

unsafe extern "system" fn drag_wheel_hook_proc(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if code >= 0 && wparam.0 as u32 == WM_MOUSEWHEEL && lparam.0 != 0 {
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        let delta = ((info.mouseData >> 16) as u16) as i16;
        let msg_raw = GLOBAL_MSG_HWND.load(Ordering::Acquire);
        if msg_raw != 0 && delta != 0 {
            unsafe {
                let _ = PostMessageW(
                    HWND(msg_raw as *mut _),
                    WM_APP_DRAG_WHEEL,
                    WPARAM((delta as u16) as usize),
                    LPARAM(0),
                );
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn install_drag_wheel_hook() {
    if DRAG_WHEEL_HOOK.load(Ordering::Acquire) != 0 {
        return;
    }
    if let Ok(hook) = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(drag_wheel_hook_proc), None, 0) }
        && !hook.is_invalid()
    {
        let prev = DRAG_WHEEL_HOOK.swap(hook.0 as isize, Ordering::AcqRel);
        if prev != 0 {
            unsafe {
                let _ = UnhookWindowsHookEx(HHOOK(prev as *mut _));
            }
        }
    }
}

fn uninstall_drag_wheel_hook() {
    let raw = DRAG_WHEEL_HOOK.swap(0, Ordering::AcqRel);
    if raw != 0 {
        unsafe {
            let _ = UnhookWindowsHookEx(HHOOK(raw as *mut _));
        }
    }
}

/// OLE drop-target attached to each visible edge pill HWND so dragging items
/// over a pill opens the stashed drawer without stealing drag foreground focus.
#[implement(IDropTarget)]
struct PillDropTarget {
    pill_hwnd_raw: isize,
    msg_hwnd_raw: isize,
}

impl IDropTarget_Impl for PillDropTarget_Impl {
    fn DragEnter(
        &self,
        _pdataobj: Option<&IDataObject>,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> WinResult<()> {
        unsafe {
            if !pdweffect.is_null() {
                *pdweffect = DROPEFFECT_NONE;
            }
            let _ = PostMessageW(
                HWND(self.msg_hwnd_raw as *mut _),
                WM_APP_DRAG_ENTER,
                WPARAM(self.pill_hwnd_raw as usize),
                LPARAM(((pt.x & 0xFFFF) | ((pt.y & 0xFFFF) << 16)) as isize),
            );
        }
        Ok(())
    }

    fn DragOver(
        &self,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> WinResult<()> {
        unsafe {
            if !pdweffect.is_null() {
                *pdweffect = DROPEFFECT_NONE;
            }
            let _ = PostMessageW(
                HWND(self.msg_hwnd_raw as *mut _),
                WM_APP_DRAG_OVER,
                WPARAM(self.pill_hwnd_raw as usize),
                LPARAM(((pt.x & 0xFFFF) | ((pt.y & 0xFFFF) << 16)) as isize),
            );
        }
        Ok(())
    }

    fn DragLeave(&self) -> WinResult<()> {
        unsafe {
            let _ = PostMessageW(
                HWND(self.msg_hwnd_raw as *mut _),
                WM_APP_DRAG_LEAVE,
                WPARAM(self.pill_hwnd_raw as usize),
                LPARAM(0),
            );
        }
        Ok(())
    }

    fn Drop(
        &self,
        _pdataobj: Option<&IDataObject>,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> WinResult<()> {
        unsafe {
            if !pdweffect.is_null() {
                *pdweffect = DROPEFFECT_NONE;
            }
            let _ = PostMessageW(
                HWND(self.msg_hwnd_raw as *mut _),
                WM_APP_DRAG_DROP,
                WPARAM(self.pill_hwnd_raw as usize),
                LPARAM(0),
            );
        }
        Ok(())
    }
}

/// Configuration options controlling how [`run_daemon_with_options`] initializes OS hooks and endpoints.
#[derive(Debug, Clone)]
pub struct DaemonLaunchOptions {
    pub pipe_name: String,
    pub config_path: PathBuf,
    pub manifest_path: PathBuf,
    pub single_instance_mutex: Option<String>,
    pub enable_tray: bool,
    pub enable_hotkeys: bool,
    pub enable_win_events: bool,
    pub enable_edge_triggers: bool,
}

impl Default for DaemonLaunchOptions {
    fn default() -> Self {
        Self {
            pipe_name: DEFAULT_PIPE_NAME.to_string(),
            config_path: EdgeStashConfig::default_path()
                .unwrap_or_else(|| PathBuf::from("edgestash.toml")),
            manifest_path: CrashManifest::default_path()
                .unwrap_or_else(|| PathBuf::from("active_stashes.json")),
            single_instance_mutex: Some(r"Local\EdgeStashDaemonMutex".to_string()),
            enable_tray: true,
            enable_hotkeys: true,
            enable_win_events: true,
            enable_edge_triggers: true,
        }
    }
}

struct ComGuard;

impl ComGuard {
    fn init() -> Self {
        unsafe {
            let _ = OleInitialize(None);
        }
        Self
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe {
            OleUninitialize();
        }
    }
}

struct MutexGuard(HANDLE);

impl Drop for MutexGuard {
    fn drop(&mut self) {
        if !self.0.is_invalid() && !self.0.0.is_null() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Performs startup recovery and registers panic/console emergency recovery hooks.
pub fn emergency_restore_from_manifest(manifest_path: &Path) -> usize {
    let sys = Win32WindowSystem::new();
    let monitors = sys.enumerate_monitors();
    let fallback_center = monitors
        .first()
        .map(|m| {
            let cx = m.work_rect.left + (m.work_rect.width() - 800).max(0) / 2;
            let cy = m.work_rect.top + (m.work_rect.height() - 600).max(0) / 2;
            Rect::new(cx, cy, cx + 800, cy + 600)
        })
        .unwrap_or(Rect::new(100, 100, 900, 700));

    recover_orphans_with(
        manifest_path,
        |hwnd| sys.is_window(hwnd) && sys.is_window_cloaked(hwnd),
        |hwnd, orig_ex, orig_rect| {
            let visible_somewhere = monitors.iter().any(|m| {
                m.rect.contains_inclusive(
                    orig_rect.left + orig_rect.width() / 2,
                    orig_rect.top + orig_rect.height() / 2,
                )
            });
            let safe_rect = if visible_somewhere && orig_rect.width() > 0 && orig_rect.height() > 0
            {
                orig_rect
            } else {
                fallback_center
            };
            sys.restore_window_from_stash(hwnd, orig_ex, safe_rect);
        },
    )
    .unwrap_or(0)
}

fn install_safety_hooks(manifest_path: PathBuf) {
    let _ = EMERGENCY_MANIFEST_PATH.set(manifest_path);
    if SAFETY_HOOKS_INSTALLED.set(()).is_err() {
        return;
    }

    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(path) = EMERGENCY_MANIFEST_PATH.get() {
            let _ = emergency_restore_from_manifest(path);
        }
        prev_hook(info);
    }));

    unsafe extern "system" fn console_ctrl_handler(ctrl_type: u32) -> BOOL {
        if matches!(
            ctrl_type,
            CTRL_C_EVENT
                | CTRL_BREAK_EVENT
                | CTRL_CLOSE_EVENT
                | CTRL_LOGOFF_EVENT
                | CTRL_SHUTDOWN_EVENT
        ) {
            if let Some(path) = EMERGENCY_MANIFEST_PATH.get() {
                let _ = emergency_restore_from_manifest(path);
            }
            let raw = GLOBAL_MSG_HWND.load(Ordering::Acquire);
            if raw != 0 {
                unsafe {
                    let _ = PostMessageW(HWND(raw as *mut _), WM_APP_IPC, WPARAM(0), LPARAM(0));
                }
            }
        }
        BOOL(0)
    }

    unsafe {
        let _ = SetConsoleCtrlHandler(Some(console_ctrl_handler), true);
    }
}

struct IpcPayload {
    req: IpcRequest,
    reply_tx: mpsc::Sender<IpcResponse>,
}

struct StripMeta {
    hwnd: HWND,
    monitor_id: String,
    edge: ScreenEdge,
    transparent: bool,
}

struct PillMeta {
    hwnd: HWND,
    surface: D2DWindowSurface,
    rect: Rect,
    slot_count: usize,
    active_index: usize,
    window_handles: Vec<WindowHandle>,
    slot_titles: Vec<String>,
    is_open: bool,
    needs_attention: Vec<bool>,
    attention_count: u32,
    visible: bool,
    drag_drop_registered: bool,
}

#[derive(Clone)]
struct DesiredPillState {
    slot_count: usize,
    active_index: usize,
    window_handles: Vec<WindowHandle>,
    slot_titles: Vec<String>,
    is_open: bool,
    needs_attention: Vec<bool>,
    attention_count: u32,
}

struct HeaderBarMeta {
    hwnd: HWND,
    surface: D2DWindowSurface,
    rect: Rect,
    title: String,
    slot_index: usize,
    slot_count: usize,
    dock_mode: DockMode,
    is_grouped: bool,
    hovered_button: Option<HeaderButtonKind>,
    visible: bool,
}

struct ShadeMeta {
    hwnd: HWND,
    surface: D2DWindowSurface,
    rect: Rect,
    visible: bool,
}

struct ActiveCloseAnim {
    icon: Option<IconPixels>,
    started_at_ms: u64,
    duration_ms: u64,
}

fn set_hwnd_ex_transparent(hwnd: HWND, transparent: bool) {
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let bit = WS_EX_TRANSPARENT.0 as isize;
        let has_bit = (ex & bit) != 0;
        if has_bit == transparent {
            return;
        }
        let next = if transparent { ex | bit } else { ex & !bit };
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, next);
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
}

struct OverlayWindowState {
    d2d: Option<D2DGraphicsContext>,
    pill_style: EdgePillStyle,
    auto_hide_pill: bool,
    dim_fade_duration_ms: u32,
    dim_fade: DimScrimFade,
    drag_transparent: bool,
    strips: HashMap<(String, ScreenEdge), StripMeta>,
    pills: HashMap<(String, ScreenEdge), PillMeta>,
    headers: HashMap<(String, ScreenEdge), HeaderBarMeta>,
    shades: HashMap<(String, ScreenEdge), ShadeMeta>,
    desired_pills: HashMap<(String, ScreenEdge), DesiredPillState>,
    drag_preview: Option<(HWND, D2DWindowSurface, Rect)>,
    dim_scrim: Option<(HWND, D2DWindowSurface, Rect)>,
    churn_previews: HashMap<ChurnZoneId, (HWND, D2DWindowSurface, Rect)>,
    icon_cache: HashMap<WindowHandle, IconPixels>,
    close_anims: HashMap<(String, ScreenEdge), ActiveCloseAnim>,
    monitors: Vec<MonitorInfo>,
    clock: SystemClock,
    msg_hwnd: HWND,
    trigger_class: Vec<u16>,
    overlay_class: Vec<u16>,
}

impl Drop for OverlayWindowState {
    fn drop(&mut self) {
        self.destroy_all_overlays();
    }
}

impl OverlayWindowState {
    fn new(
        msg_hwnd: HWND,
        pill_style: EdgePillStyle,
        auto_hide_pill: bool,
        dim_fade_duration_ms: u32,
        monitors: Vec<MonitorInfo>,
    ) -> Self {
        Self {
            d2d: D2DGraphicsContext::new().ok(),
            pill_style,
            auto_hide_pill,
            dim_fade_duration_ms,
            dim_fade: DimScrimFade::default(),
            drag_transparent: false,
            strips: HashMap::new(),
            pills: HashMap::new(),
            headers: HashMap::new(),
            shades: HashMap::new(),
            desired_pills: HashMap::new(),
            drag_preview: None,
            dim_scrim: None,
            churn_previews: HashMap::new(),
            icon_cache: HashMap::new(),
            close_anims: HashMap::new(),
            monitors,
            clock: SystemClock::new(),
            msg_hwnd,
            trigger_class: to_wide_null("EdgeStashTriggerStrip"),
            overlay_class: to_wide_null("EdgeStashD2DOverlay"),
        }
    }

    fn destroy_all_overlays(&mut self) {
        for (_, strip) in self.strips.drain() {
            unsafe {
                let _ = DestroyWindow(strip.hwnd);
            }
        }
        for (_, pill) in self.pills.drain() {
            unsafe {
                if pill.drag_drop_registered {
                    let _ = RevokeDragDrop(pill.hwnd);
                }
                let _ = DestroyWindow(pill.hwnd);
            }
        }
        for (_, header) in self.headers.drain() {
            unsafe {
                let _ = DestroyWindow(header.hwnd);
            }
        }
        for (_, shade) in self.shades.drain() {
            unsafe {
                let _ = DestroyWindow(shade.hwnd);
            }
        }
        if let Some((hwnd, _, _)) = self.drag_preview.take() {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        }
        if let Some((hwnd, _, _)) = self.dim_scrim.take() {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        }
        for (_, (hwnd, _, _)) in self.churn_previews.drain() {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        }
    }

    fn all_overlay_hwnds(&self) -> Vec<WindowHandle> {
        let mut out = Vec::with_capacity(
            self.strips.len()
                + self.pills.len()
                + self.headers.len()
                + self.shades.len()
                + self.churn_previews.len()
                + 3,
        );
        out.push(WindowHandle::from_hwnd(self.msg_hwnd));
        for s in self.strips.values() {
            out.push(WindowHandle::from_hwnd(s.hwnd));
        }
        for p in self.pills.values() {
            out.push(WindowHandle::from_hwnd(p.hwnd));
        }
        for h in self.headers.values() {
            out.push(WindowHandle::from_hwnd(h.hwnd));
        }
        for sh in self.shades.values() {
            out.push(WindowHandle::from_hwnd(sh.hwnd));
        }
        for (h, _, _) in self.churn_previews.values() {
            out.push(WindowHandle::from_hwnd(*h));
        }
        if let Some((h, _, _)) = &self.drag_preview {
            out.push(WindowHandle::from_hwnd(*h));
        }
        if let Some((h, _, _)) = &self.dim_scrim {
            out.push(WindowHandle::from_hwnd(*h));
        }
        out
    }

    fn current_open_floating_hwnds(&self) -> Vec<WindowHandle> {
        let mut out = Vec::new();
        for (key, desired) in &self.desired_pills {
            if !desired.is_open {
                continue;
            }
            let is_native_dock = self
                .headers
                .get(key)
                .is_some_and(|h| h.dock_mode == DockMode::NativeWorkAreaDock);
            if !is_native_dock {
                out.extend(desired.window_handles.iter().copied());
            }
        }
        out
    }

    fn rebuild_trigger_strips(&mut self, enable_triggers: bool) {
        for (_, strip) in self.strips.drain() {
            unsafe {
                let _ = DestroyWindow(strip.hwnd);
            }
        }
        if !enable_triggers {
            return;
        }
        for mon in &self.monitors {
            for edge in ScreenEdge::ALL {
                let r = trigger_strip_rect(mon, edge, 1);
                if let Ok(hwnd) = create_trigger_strip_window(&self.trigger_class, r, self.msg_hwnd)
                {
                    self.strips.insert(
                        (mon.id.clone(), edge),
                        StripMeta {
                            hwnd,
                            monitor_id: mon.id.clone(),
                            edge,
                            transparent: true,
                        },
                    );
                }
            }
        }
    }

    fn set_strip_transparent(&mut self, mon_id: &str, edge: ScreenEdge, transparent: bool) {
        if let Some(strip) = self.strips.get_mut(&(mon_id.to_string(), edge))
            && strip.transparent != transparent
        {
            strip.transparent = transparent;
            set_hwnd_ex_transparent(strip.hwnd, transparent);
        }
    }

    fn set_drag_transparent(&mut self, drag: bool) {
        self.drag_transparent = drag;
        if let Some((hwnd, _, _)) = &self.dim_scrim {
            set_hwnd_ex_transparent(*hwnd, drag);
        }
    }

    fn ensure_overlays_topmost(&self) {
        let open_floating = self.current_open_floating_hwnds();
        unsafe {
            // Raise floating drawers first so we can atomically insert dim_scrim BELOW the bottommost one
            // without ever popping scrim onto HWND_TOPMOST above visible drawers (eliminates Bug #1 flicker).
            for h in &open_floating {
                let _ = SetWindowPos(
                    h.to_hwnd(),
                    HWND_TOPMOST,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
            if self.dim_fade.is_visible()
                && let Some((scrim_hwnd, _, _)) = &self.dim_scrim
            {
                let insert_after = compute_dim_scrim_insert_after(&open_floating)
                    .map(|h| h.to_hwnd())
                    .unwrap_or(HWND_TOPMOST);
                let _ = SetWindowPos(
                    *scrim_hwnd,
                    insert_after,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
            for header in self.headers.values() {
                if header.visible {
                    let insert_after = if header.dock_mode == DockMode::NativeWorkAreaDock {
                        HWND_NOTOPMOST
                    } else {
                        HWND_TOPMOST
                    };
                    let _ = SetWindowPos(
                        header.hwnd,
                        insert_after,
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                }
            }
            for pill in self.pills.values() {
                if pill.visible {
                    let _ = SetWindowPos(
                        pill.hwnd,
                        HWND_TOPMOST,
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                }
            }
            for shade in self.shades.values() {
                if shade.visible {
                    let _ = SetWindowPos(
                        shade.hwnd,
                        HWND_TOPMOST,
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                }
            }
            for strip in self.strips.values() {
                let _ = SetWindowPos(
                    strip.hwnd,
                    HWND_TOPMOST,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
            for (h, _, _) in self.churn_previews.values() {
                let _ = SetWindowPos(
                    *h,
                    HWND_TOPMOST,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
            bring_drag_image_to_top();
        }
    }

    fn find_strip_by_hwnd(&self, hwnd: HWND) -> Option<(String, ScreenEdge)> {
        self.strips
            .values()
            .find(|s| s.hwnd == hwnd)
            .map(|s| (s.monitor_id.clone(), s.edge))
    }

    fn find_pill_by_hwnd(&self, hwnd: HWND) -> Option<(String, ScreenEdge)> {
        self.pills
            .iter()
            .find(|(_, p)| p.hwnd == hwnd)
            .map(|(k, _)| k.clone())
    }

    fn find_header_by_hwnd(&self, hwnd: HWND) -> Option<(String, ScreenEdge)> {
        self.headers
            .iter()
            .find(|(_, h)| h.hwnd == hwnd)
            .map(|(k, _)| k.clone())
    }

    fn is_scrim_hwnd(&self, hwnd: HWND) -> bool {
        self.dim_scrim.as_ref().is_some_and(|(h, _, _)| *h == hwnd)
    }

    fn slot_count_for(&self, monitor_id: &str, edge: ScreenEdge) -> usize {
        self.desired_pills
            .get(&(monitor_id.to_string(), edge))
            .map(|d| d.slot_count)
            .unwrap_or(0)
    }

    fn render_header_meta(h: &mut HeaderBarMeta) {
        let visual = DrawerHeaderVisual {
            title: &h.title,
            slot_index: h.slot_index,
            slot_count: h.slot_count,
            dock_mode: h.dock_mode,
            hovered_button: h.hovered_button,
        };
        let _ = h.surface.render_drawer_header(
            h.rect.width().max(1) as u32,
            h.rect.height().max(1) as u32,
            &visual,
        );
    }

    fn render_dim_scrim_current(&mut self) {
        let alpha = self.dim_fade.current_alpha();
        if let Some((hwnd, surface, rect)) = &mut self.dim_scrim {
            if alpha <= 0.001 && !self.dim_fade.is_visible() {
                unsafe {
                    let _ = ShowWindow(*hwnd, SW_HIDE);
                }
                return;
            }
            let _ = surface.render_scrim_alpha(
                rect.width().max(1) as u32,
                rect.height().max(1) as u32,
                alpha,
            );
        }
    }

    fn tick_dim_fade(&mut self) -> bool {
        let now = self.clock.now_ms();
        let (_alpha, still_running) = self.dim_fade.advance(now);
        self.render_dim_scrim_current();
        if !still_running
            && !self.dim_fade.is_visible()
            && let Some((hwnd, _, _)) = &self.dim_scrim
        {
            unsafe {
                let _ = ShowWindow(*hwnd, SW_HIDE);
            }
        }
        still_running
    }

    fn set_churn_previews(
        &mut self,
        enabled: bool,
        target_zone: Option<ChurnZoneId>,
        corner_zone_px: i32,
        slide_distance_px: i32,
        diagonal_angle_deg: i32,
    ) {
        if !enabled {
            for (_, (hwnd, _, _)) in self.churn_previews.drain() {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            }
            return;
        }

        let Some(mon) = self
            .monitors
            .iter()
            .find(|m| m.is_primary)
            .cloned()
            .or_else(|| self.monitors.first().cloned())
        else {
            return;
        };

        let zones_to_render: Vec<ChurnZoneId> = match target_zone {
            Some(z) => vec![z],
            None => vec![ChurnZoneId::TopRightDown, ChurnZoneId::BottomRightUp],
        };

        // Clean up any overlays not part of the active selection
        let existing_zones: Vec<ChurnZoneId> = self.churn_previews.keys().copied().collect();
        for z in existing_zones {
            if !zones_to_render.contains(&z)
                && let Some((hwnd, _, _)) = self.churn_previews.remove(&z)
            {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            }
        }

        for zone in zones_to_render {
            let target_rect = compute_churn_preview_rect(
                &mon,
                zone,
                corner_zone_px,
                slide_distance_px,
                diagonal_angle_deg,
            );

            if let Some((hwnd, surface, rect)) = self.churn_previews.get_mut(&zone) {
                if *rect != target_rect {
                    *rect = target_rect;
                    unsafe {
                        let _ = SetWindowPos(
                            *hwnd,
                            HWND_TOPMOST,
                            target_rect.left,
                            target_rect.top,
                            target_rect.width().max(1),
                            target_rect.height().max(1),
                            SWP_NOACTIVATE,
                        );
                    }
                }
                let _ = surface.render_churn_zone_visual(
                    target_rect.width().max(1) as u32,
                    target_rect.height().max(1) as u32,
                    zone,
                    corner_zone_px,
                    slide_distance_px,
                    diagonal_angle_deg,
                );
                unsafe {
                    let _ = ShowWindow(*hwnd, SW_SHOWNOACTIVATE);
                }
            } else if let Some(ctx) = &self.d2d
                && let Ok(hwnd) =
                    create_d2d_overlay_window(&self.overlay_class, target_rect, true, self.msg_hwnd)
            {
                if let Ok(mut surface) = ctx.create_window_surface(
                    hwnd,
                    target_rect.width().max(1) as u32,
                    target_rect.height().max(1) as u32,
                ) {
                    let _ = surface.render_churn_zone_visual(
                        target_rect.width().max(1) as u32,
                        target_rect.height().max(1) as u32,
                        zone,
                        corner_zone_px,
                        slide_distance_px,
                        diagonal_angle_deg,
                    );
                    unsafe {
                        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    }
                    self.churn_previews
                        .insert(zone, (hwnd, surface, target_rect));
                } else {
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                }
            }
        }
    }

    fn sync_pill(&mut self, monitor_id: &str, edge: ScreenEdge) {
        let Some(mon) = self.monitors.iter().find(|m| m.id == monitor_id).cloned() else {
            return;
        };
        let key = (monitor_id.to_string(), edge);
        let anim_progress = self.close_anims.get(&key).map(|a| {
            let elapsed = self.clock.now_ms().saturating_sub(a.started_at_ms);
            (elapsed as f32 / a.duration_ms as f32).clamp(0.0, 1.0)
        });
        let is_animating = anim_progress.is_some();

        let desired = self
            .desired_pills
            .get(&key)
            .cloned()
            .unwrap_or(DesiredPillState {
                slot_count: 0,
                active_index: 0,
                window_handles: Vec::new(),
                slot_titles: Vec::new(),
                is_open: false,
                needs_attention: Vec::new(),
                attention_count: 0,
            });

        let should_show = should_pill_be_visible(
            self.pill_style,
            self.auto_hide_pill,
            desired.slot_count,
            desired.is_open,
            is_animating,
            desired.attention_count > 0,
        );

        if !should_show {
            if let Some(pill) = self.pills.get_mut(&key)
                && pill.visible
            {
                unsafe {
                    let _ = ShowWindow(pill.hwnd, SW_HIDE);
                }
                pill.visible = false;
            }
            return;
        }

        let target_rect = if !desired.is_open && desired.attention_count > 0 {
            edge_pill_rect_for_style(&mon, edge, EdgePillStyle::AppIconsStack, 1)
        } else {
            edge_pill_rect_for_style(&mon, edge, self.pill_style, desired.slot_count.max(1))
        };

        if !self.pills.contains_key(&key)
            && let Some(ctx) = &self.d2d
            && let Ok(hwnd) =
                create_d2d_overlay_window(&self.overlay_class, target_rect, false, self.msg_hwnd)
        {
            if let Ok(surface) = ctx.create_window_surface(
                hwnd,
                target_rect.width().max(1) as u32,
                target_rect.height().max(1) as u32,
            ) {
                let drop_target: IDropTarget = PillDropTarget {
                    pill_hwnd_raw: hwnd.0 as isize,
                    msg_hwnd_raw: self.msg_hwnd.0 as isize,
                }
                .into();
                let drag_drop_registered = unsafe { RegisterDragDrop(hwnd, &drop_target).is_ok() };
                self.pills.insert(
                    key.clone(),
                    PillMeta {
                        hwnd,
                        surface,
                        rect: target_rect,
                        slot_count: desired.slot_count,
                        active_index: desired.active_index,
                        window_handles: desired.window_handles.clone(),
                        slot_titles: desired.slot_titles.clone(),
                        is_open: desired.is_open,
                        needs_attention: desired.needs_attention.clone(),
                        attention_count: desired.attention_count,
                        visible: false,
                        drag_drop_registered,
                    },
                );
            } else {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            }
        }

        let close_anim_owned = self
            .close_anims
            .get(&key)
            .and_then(|a| anim_progress.map(|p| (p, a.icon.clone())));
        let pill_style = self.pill_style;
        let visuals_owned: Vec<(Option<&IconPixels>, String)> = desired
            .window_handles
            .iter()
            .enumerate()
            .map(|(i, h)| {
                let icon = self.icon_cache.get(h).filter(|px| px.width > 1);
                let title = desired.slot_titles.get(i).cloned().unwrap_or_default();
                (icon, title)
            })
            .collect();
        let visuals: Vec<PillSlotVisual<'_>> = visuals_owned
            .iter()
            .enumerate()
            .map(|(i, (icon, title))| PillSlotVisual {
                icon: *icon,
                title: title.as_str(),
                needs_attention: desired.needs_attention.get(i).copied().unwrap_or(false),
            })
            .collect();

        if let Some(pill) = self.pills.get_mut(&key) {
            pill.slot_count = desired.slot_count;
            pill.active_index = desired.active_index;
            pill.window_handles = desired.window_handles;
            pill.slot_titles = desired.slot_titles;
            pill.is_open = desired.is_open;
            pill.needs_attention = desired.needs_attention.clone();
            pill.attention_count = desired.attention_count;
            pill.rect = target_rect;
            unsafe {
                let _ = SetWindowPos(
                    pill.hwnd,
                    HWND_TOPMOST,
                    pill.rect.left,
                    pill.rect.top,
                    pill.rect.width().max(1),
                    pill.rect.height().max(1),
                    SWP_NOACTIVATE,
                );
                let _ = ShowWindow(pill.hwnd, SW_SHOWNOACTIVATE);
            }
            pill.visible = true;
            let frame = close_anim_owned.as_ref().map(|(p, icon)| CloseAnimFrame {
                progress: *p,
                icon: icon.as_ref(),
            });
            let alert_idx = desired.needs_attention.iter().position(|&a| a).unwrap_or(0);
            let now_ms = self.clock.now_ms();
            let (render_visuals, render_slot_count, render_active_index) =
                if !desired.is_open && desired.attention_count > 0 {
                    let single_visual = visuals
                        .get(alert_idx)
                        .cloned()
                        .into_iter()
                        .collect::<Vec<_>>();
                    (single_visual, 1, 0)
                } else {
                    (visuals, pill.slot_count, pill.active_index)
                };
            let _ = pill.surface.render_pill(
                pill.rect.width().max(1) as u32,
                pill.rect.height().max(1) as u32,
                pill_style,
                &render_visuals,
                render_slot_count,
                render_active_index,
                frame,
                desired.attention_count,
                now_ms,
            );
        }
        self.ensure_overlays_topmost();
    }

    fn tick_close_animations(&mut self) -> bool {
        let now = self.clock.now_ms();
        let finished: Vec<(String, ScreenEdge)> = self
            .close_anims
            .iter()
            .filter(|&(_k, anim)| now.saturating_sub(anim.started_at_ms) >= anim.duration_ms)
            .map(|(k, _anim)| k.clone())
            .collect();

        for k in finished {
            self.close_anims.remove(&k);
            self.sync_pill(&k.0, k.1);
        }

        let remaining: Vec<(String, ScreenEdge)> = self.close_anims.keys().cloned().collect();
        for k in remaining {
            self.sync_pill(&k.0, k.1);
        }
        !self.close_anims.is_empty()
    }

    fn set_area_scroll_shade(&mut self, monitor_id: &str, edge: ScreenEdge, visible: bool) {
        let key = (monitor_id.to_string(), edge);
        if !visible {
            if let Some(shade) = self.shades.get_mut(&key)
                && shade.visible
            {
                shade.visible = false;
                unsafe {
                    let _ = ShowWindow(shade.hwnd, SW_HIDE);
                }
            }
            return;
        }
        let Some(mon) = self.monitors.iter().find(|m| m.id == monitor_id).cloned() else {
            return;
        };
        let target_rect = area_scroll_shade_rect(&mon, edge);
        if !self.shades.contains_key(&key)
            && let Some(ctx) = &self.d2d
            && let Ok(hwnd) =
                create_d2d_overlay_window(&self.overlay_class, target_rect, true, self.msg_hwnd)
        {
            if let Ok(surface) = ctx.create_window_surface(
                hwnd,
                target_rect.width().max(1) as u32,
                target_rect.height().max(1) as u32,
            ) {
                self.shades.insert(
                    key.clone(),
                    ShadeMeta {
                        hwnd,
                        surface,
                        rect: target_rect,
                        visible: false,
                    },
                );
            } else {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            }
        }
        if let Some(shade) = self.shades.get_mut(&key) {
            let needs_redraw = !shade.visible || shade.rect != target_rect;
            shade.rect = target_rect;
            if needs_redraw {
                let _ = shade.surface.render_area_scroll_shade(
                    target_rect.width().max(1) as u32,
                    target_rect.height().max(1) as u32,
                    edge,
                );
                unsafe {
                    let _ = SetWindowPos(
                        shade.hwnd,
                        HWND_TOPMOST,
                        target_rect.left,
                        target_rect.top,
                        target_rect.width().max(1),
                        target_rect.height().max(1),
                        SWP_NOACTIVATE,
                    );
                    let _ = ShowWindow(shade.hwnd, SW_SHOWNOACTIVATE);
                }
                shade.visible = true;
            }
        }
    }
}

#[derive(Clone)]
struct SharedOverlayBridge {
    inner: Rc<RefCell<OverlayWindowState>>,
}

impl VisualOverlayApi for SharedOverlayBridge {
    fn header_hwnd_for_edge(&self, monitor_id: &str, edge: ScreenEdge) -> Option<WindowHandle> {
        self.inner
            .borrow()
            .headers
            .get(&(monitor_id.to_string(), edge))
            .map(|h| WindowHandle::from_hwnd(h.hwnd))
    }

    fn emit_overlay_event(&self, event: PillOverlayEvent) {
        let mut st = self.inner.borrow_mut();
        match event {
            PillOverlayEvent::SyncState {
                monitor_id,
                edge,
                slot_count,
                active_index,
                window_handles,
                slot_titles,
                is_open,
                needs_attention,
                attention_count,
            } => {
                for hwnd in &window_handles {
                    st.icon_cache.entry(*hwnd).or_insert_with(|| IconPixels {
                        width: 1,
                        height: 1,
                        bgra: vec![0, 0, 0, 0],
                    });
                    if st.icon_cache.get(hwnd).is_some_and(|i| i.width == 1)
                        && let Some(real_icon) = extract_window_icon_bgra(hwnd.to_hwnd())
                    {
                        st.icon_cache.insert(*hwnd, real_icon);
                    }
                }

                let key = (monitor_id.clone(), edge);
                if slot_count == 0 {
                    st.desired_pills.remove(&key);
                } else {
                    st.desired_pills.insert(
                        key,
                        DesiredPillState {
                            slot_count,
                            active_index,
                            window_handles,
                            slot_titles,
                            is_open,
                            needs_attention,
                            attention_count,
                        },
                    );
                }
                if attention_count > 0 {
                    unsafe {
                        let _ = SetTimer(st.msg_hwnd, TIMER_ATTENTION_PULSE, 33, None);
                    }
                }
                st.sync_pill(&monitor_id, edge);
            }
            PillOverlayEvent::WindowClosedAnimation {
                monitor_id,
                edge,
                closed_hwnd,
                closed_title: _,
            } => {
                let icon = st.icon_cache.remove(&closed_hwnd).filter(|i| i.width > 1);
                let started_at_ms = st.clock.now_ms();
                let key = (monitor_id.clone(), edge);
                st.close_anims.insert(
                    key,
                    ActiveCloseAnim {
                        icon,
                        started_at_ms,
                        duration_ms: 550,
                    },
                );
                st.sync_pill(&monitor_id, edge);
                unsafe {
                    let _ = SetTimer(st.msg_hwnd, TIMER_CLOSE_ANIM, 16, None);
                }
            }
            PillOverlayEvent::ShowDragPreview {
                monitor_id: _,
                edge,
                band_rect,
                active_target,
                edge_zone_px,
                corner_span_px,
            } => {
                if st.drag_preview.is_none()
                    && let Some(ctx) = &st.d2d
                    && let Ok(hwnd) =
                        create_d2d_overlay_window(&st.overlay_class, band_rect, true, st.msg_hwnd)
                {
                    if let Ok(surface) = ctx.create_window_surface(
                        hwnd,
                        band_rect.width().max(1) as u32,
                        band_rect.height().max(1) as u32,
                    ) {
                        st.drag_preview = Some((hwnd, surface, band_rect));
                    } else {
                        unsafe {
                            let _ = DestroyWindow(hwnd);
                        }
                    }
                }
                let d2d_ctx = st.d2d.clone();
                if let (Some((hwnd, surface, rect)), Some(ctx)) = (&mut st.drag_preview, &d2d_ctx) {
                    let size_changed =
                        rect.width() != band_rect.width() || rect.height() != band_rect.height();
                    *rect = band_rect;
                    unsafe {
                        let _ = SetWindowPos(
                            *hwnd,
                            HWND_TOPMOST,
                            band_rect.left,
                            band_rect.top,
                            band_rect.width().max(1),
                            band_rect.height().max(1),
                            SWP_NOACTIVATE,
                        );
                        let _ = ShowWindow(*hwnd, SW_SHOWNOACTIVATE);
                    }
                    if size_changed
                        && let Ok(new_surface) = ctx.create_window_surface(
                            *hwnd,
                            band_rect.width().max(1) as u32,
                            band_rect.height().max(1) as u32,
                        )
                    {
                        *surface = new_surface;
                    }
                    let _ = surface.render_drag_preview(
                        band_rect.width().max(1) as u32,
                        band_rect.height().max(1) as u32,
                        edge,
                        edge_zone_px,
                        corner_span_px,
                        active_target.as_ref(),
                    );
                }
            }
            PillOverlayEvent::HideDragPreview => {
                if let Some((hwnd, _, _)) = &st.drag_preview {
                    unsafe {
                        let _ = ShowWindow(*hwnd, SW_HIDE);
                    }
                }
            }
            PillOverlayEvent::SetDimScrim {
                visible,
                monitor_rect,
                opacity_percent,
            } => {
                if visible
                    && st.dim_scrim.is_none()
                    && let Some(ctx) = &st.d2d
                    && let Ok(hwnd) = create_d2d_overlay_window(
                        &st.overlay_class,
                        monitor_rect,
                        st.drag_transparent,
                        st.msg_hwnd,
                    )
                {
                    if let Ok(surface) = ctx.create_window_surface(
                        hwnd,
                        monitor_rect.width().max(1) as u32,
                        monitor_rect.height().max(1) as u32,
                    ) {
                        st.dim_scrim = Some((hwnd, surface, monitor_rect));
                    } else {
                        unsafe {
                            let _ = DestroyWindow(hwnd);
                        }
                    }
                }
                if let Some((_, _, rect)) = &mut st.dim_scrim
                    && visible
                {
                    *rect = monitor_rect;
                }
                let target_alpha = if visible {
                    (opacity_percent.min(100) as f32) / 100.0
                } else {
                    0.0
                };
                let now_ms = st.clock.now_ms();
                let fade_ms = st.dim_fade_duration_ms;
                st.dim_fade.set_target(target_alpha, fade_ms, now_ms);
                let (_alpha, animating) = st.dim_fade.advance(now_ms);

                if st.dim_fade.is_visible() {
                    if let Some((hwnd, _, rect)) = &st.dim_scrim {
                        unsafe {
                            let _ = SetWindowPos(
                                *hwnd,
                                None,
                                rect.left,
                                rect.top,
                                rect.width().max(1),
                                rect.height().max(1),
                                SWP_NOZORDER | SWP_NOACTIVATE,
                            );
                        }
                        set_hwnd_ex_transparent(*hwnd, st.drag_transparent);
                    }
                    st.render_dim_scrim_current();
                    st.ensure_overlays_topmost();
                    if let Some((hwnd, _, _)) = &st.dim_scrim {
                        unsafe {
                            let _ = ShowWindow(*hwnd, SW_SHOWNOACTIVATE);
                        }
                    }
                } else if let Some((hwnd, _, _)) = &st.dim_scrim {
                    unsafe {
                        let _ = ShowWindow(*hwnd, SW_HIDE);
                    }
                }

                unsafe {
                    if !animating {
                        let _ = KillTimer(st.msg_hwnd, TIMER_DIM_FADE);
                    } else {
                        let _ = SetTimer(st.msg_hwnd, TIMER_DIM_FADE, 16, None);
                    }
                }
            }
            PillOverlayEvent::ShowDrawerHeader {
                monitor_id,
                edge,
                header_rect,
                title,
                slot_index,
                slot_count,
                dock_mode,
                preferred_pin_mode: _,
                is_grouped,
            } => {
                let key = (monitor_id, edge);
                if !st.headers.contains_key(&key)
                    && let Some(ctx) = &st.d2d
                    && let Ok(hwnd) = create_d2d_overlay_window(
                        &st.overlay_class,
                        header_rect,
                        false,
                        st.msg_hwnd,
                    )
                {
                    if let Ok(surface) = ctx.create_window_surface(
                        hwnd,
                        header_rect.width().max(1) as u32,
                        header_rect.height().max(1) as u32,
                    ) {
                        st.headers.insert(
                            key.clone(),
                            HeaderBarMeta {
                                hwnd,
                                surface,
                                rect: header_rect,
                                title: title.clone(),
                                slot_index,
                                slot_count,
                                dock_mode,
                                is_grouped,
                                hovered_button: None,
                                visible: false,
                            },
                        );
                    } else {
                        unsafe {
                            let _ = DestroyWindow(hwnd);
                        }
                    }
                }
                if let Some(h) = st.headers.get_mut(&key) {
                    h.rect = header_rect;
                    h.title = title;
                    h.slot_index = slot_index;
                    h.slot_count = slot_count;
                    h.dock_mode = dock_mode;
                    h.is_grouped = is_grouped;
                    if !h.visible {
                        h.hovered_button = None;
                    }
                    let insert_after = if dock_mode == DockMode::NativeWorkAreaDock {
                        HWND_NOTOPMOST
                    } else {
                        HWND_TOPMOST
                    };
                    unsafe {
                        let _ = SetWindowPos(
                            h.hwnd,
                            insert_after,
                            header_rect.left,
                            header_rect.top,
                            header_rect.width().max(1),
                            header_rect.height().max(1),
                            SWP_NOACTIVATE,
                        );
                        let _ = ShowWindow(h.hwnd, SW_SHOWNOACTIVATE);
                    }
                    h.visible = true;
                    OverlayWindowState::render_header_meta(h);
                }
                st.ensure_overlays_topmost();
            }
            PillOverlayEvent::HideDrawerHeader { monitor_id, edge } => {
                if let Some(h) = st.headers.get_mut(&(monitor_id, edge)) {
                    h.visible = false;
                    h.hovered_button = None;
                    unsafe {
                        let _ = ShowWindow(h.hwnd, SW_HIDE);
                    }
                }
            }
        }
    }
}

fn create_trigger_strip_window(
    class_name: &[u16],
    rect: Rect,
    owner_msg: HWND,
) -> windows::core::Result<HWND> {
    let hinst: HINSTANCE = unsafe { GetModuleHandleW(None)? }.into();
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT,
            PCWSTR(class_name.as_ptr()),
            w!("EdgeStashStrip"),
            WS_POPUP,
            rect.left,
            rect.top,
            rect.width().max(1),
            rect.height().max(1),
            None,
            None,
            hinst,
            Some(owner_msg.0),
        )?
    };
    unsafe {
        // Alpha = 1 keeps window invisible while preserving Win32 WM_MOUSEMOVE hit-testing!
        let _ = windows::Win32::UI::WindowsAndMessaging::SetLayeredWindowAttributes(
            hwnd,
            windows::Win32::Foundation::COLORREF(0),
            1,
            windows::Win32::UI::WindowsAndMessaging::LWA_ALPHA,
        );
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    Ok(hwnd)
}

fn create_d2d_overlay_window(
    class_name: &[u16],
    rect: Rect,
    click_through: bool,
    owner_msg: HWND,
) -> windows::core::Result<HWND> {
    let hinst: HINSTANCE = unsafe { GetModuleHandleW(None)? }.into();
    let mut ex = WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
    if click_through {
        ex |= WS_EX_TRANSPARENT;
    }
    unsafe {
        CreateWindowExW(
            ex,
            PCWSTR(class_name.as_ptr()),
            w!("EdgeStashOverlay"),
            WS_POPUP,
            rect.left,
            rect.top,
            rect.width().max(1),
            rect.height().max(1),
            None,
            None,
            hinst,
            Some(owner_msg.0),
        )
    }
}

fn append_menu_item(
    menu: windows::Win32::UI::WindowsAndMessaging::HMENU,
    id: u32,
    label: &str,
    checked: bool,
) {
    let wide = to_wide_null(label);
    let flags = if checked {
        MF_STRING | MF_CHECKED
    } else {
        MF_STRING
    };
    unsafe {
        let _ = AppendMenuW(menu, flags, id as usize, PCWSTR(wide.as_ptr()));
    }
}

fn hit_test_header_button(rect: Rect, lparam: LPARAM) -> Option<HeaderButtonKind> {
    let cx = (lparam.0 & 0xFFFF) as i16 as i32;
    let cy = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
    header_button_rects(rect.width(), rect.height())
        .into_iter()
        .find(|(_, btn_rect)| btn_rect.contains_inclusive(cx, cy))
        .map(|(kind, _)| kind)
}

struct DaemonController {
    engine:
        EdgeStashEngine<Win32WindowSystem, Win32VirtualDesktop, Win32AppBar, SharedOverlayBridge>,
    overlays: Rc<RefCell<OverlayWindowState>>,
    clock: SystemClock,
    msg_hwnd: HWND,
    config_path: PathBuf,
    charms: HashMap<String, DirectedCharmsDetector>,
    bumps: HashMap<(String, ScreenEdge), EdgeBumpStateMachine>,
    hovers: HashMap<(String, ScreenEdge), EdgeHoverStateMachine>,
    area_scrolls: HashMap<(String, ScreenEdge), AreaAwareScrollController>,
    prev_open_edges: HashSet<(String, ScreenEdge)>,
    cursor_on_line_edges: HashSet<(String, ScreenEdge)>,
    last_opened_via_edge: Option<(String, ScreenEdge)>,
    active_hover: Option<(String, ScreenEdge, bool, bool)>,
    drag_hover_pill: Option<(String, ScreenEdge)>,
    drag_open_active: bool,
    drag_capture: DragCaptureStateMachine,
    hotkeys_enabled: bool,
    edge_triggers_enabled: bool,
    tray_active: bool,
    tray_retries_remaining: u32,
    should_quit: bool,
}

impl DaemonController {
    fn sync_hotkeys(&self) {
        if !self.hotkeys_enabled {
            return;
        }
        for id in HOTKEY_STASH_LEFT..=HOTKEY_UNSTASH_ACTIVE {
            unsafe {
                let _ = UnregisterHotKey(self.msg_hwnd, id);
            }
        }
        let hk = &self.engine.config().hotkeys;
        let bindings = [
            (HOTKEY_STASH_LEFT, hk.stash_left.as_str()),
            (HOTKEY_STASH_RIGHT, hk.stash_right.as_str()),
            (HOTKEY_STASH_TOP, hk.stash_top.as_str()),
            (HOTKEY_STASH_BOTTOM, hk.stash_bottom.as_str()),
            (HOTKEY_TOGGLE_ACTIVE, hk.toggle_active.as_str()),
            (HOTKEY_GROUP_ACTIVE, hk.group_active.as_str()),
            (HOTKEY_UNSTASH_ACTIVE, hk.unstash_active.as_str()),
        ];
        for (id, raw) in bindings {
            if let Ok(spec) = HotkeySpec::parse(raw) {
                unsafe {
                    let _ = RegisterHotKey(
                        self.msg_hwnd,
                        id,
                        HOT_KEY_MODIFIERS(spec.win32_modifiers()),
                        spec.vk_code(),
                    );
                }
            }
        }
    }

    fn reload_config(&mut self) -> Result<(), String> {
        let new_cfg = EdgeStashConfig::load_or_create_at(&self.config_path)
            .map_err(|err| format!("Failed to reload config: {err}"))?;
        {
            let mut ov = self.overlays.borrow_mut();
            ov.pill_style = new_cfg.edge_pill_style;
            ov.auto_hide_pill = new_cfg.auto_hide_pill;
            ov.dim_fade_duration_ms = new_cfg.dim_fade_duration_ms;
        }
        self.engine.update_config(new_cfg);
        self.charms.clear();
        self.bumps.clear();
        self.hovers.clear();
        self.active_hover = None;
        self.sync_hotkeys();
        let keys: Vec<(String, ScreenEdge)> =
            self.overlays.borrow().pills.keys().cloned().collect();
        for (mon, edge) in keys {
            self.overlays.borrow_mut().sync_pill(&mon, edge);
        }
        self.overlays
            .borrow_mut()
            .rebuild_trigger_strips(self.edge_triggers_enabled);
        self.cursor_on_line_edges.clear();
        self.sync_overlay_handles_to_engine();
        self.sync_ll_mouse_snapshot();
        self.update_dismiss_timer();
        Ok(())
    }

    fn update_dismiss_timer(&self) {
        let snap = self.engine.status_snapshot(std::process::id());
        let has_open = snap.slots.iter().any(|s| s.is_open);
        let has_pending = self.engine.has_pending_focus_restore();
        unsafe {
            if has_open || self.drag_open_active || has_pending {
                let _ = SetTimer(self.msg_hwnd, TIMER_DISMISS_CHECK, 33, None);
            } else {
                let _ = KillTimer(self.msg_hwnd, TIMER_DISMISS_CHECK);
            }
        }
    }

    fn sync_strip_hit_test(&mut self, mon_id: &str, edge: ScreenEdge) {
        let cfg = self.engine.config();
        let is_open = self.engine.is_edge_open(mon_id, edge);
        let has_slots = self.engine.has_slots_on_edge(mon_id, edge);
        let transparent = is_strip_input_transparent(
            cfg.gesture_mode,
            cfg.area_aware_edge_scroll,
            cfg.area_aware_edge_scroll_mode,
            is_open,
            has_slots,
        ) || self.edge_triggers_enabled;
        // Note: when global mouse hook is active, strip HWNDs remain click/scroll-transparent so
        // right-edge scrollbars are never blocked, while WH_MOUSE_LL intercepts wheel when armed.
        self.overlays
            .borrow_mut()
            .set_strip_transparent(mon_id, edge, transparent);
    }

    fn sync_open_stashed_hwnds(&self) {
        let open = self.engine.open_stashed_hwnds();
        OPEN_STASHED_HWNDS.with(|cell| {
            let mut set = cell.borrow_mut();
            set.clear();
            set.extend(open);
        });
    }

    fn sync_ll_mouse_snapshot(&self) {
        let cfg = self.engine.config();
        let monitors = self.overlays.borrow().monitors.clone();
        let any_open = self.engine.has_any_open_slot();
        let mut wheel_capture_zones = Vec::new();

        if self.drag_hover_pill.is_none() && cfg.area_aware_edge_scroll {
            for mon in &monitors {
                for edge in ScreenEdge::ALL {
                    let is_open = self.engine.is_edge_open(&mon.id, edge);
                    let has_slots = self.engine.has_slots_on_edge(&mon.id, edge);
                    let active =
                        self.area_scrolls
                            .get(&(mon.id.clone(), edge))
                            .is_some_and(|ctrl| {
                                ctrl.should_strip_capture_pointer(
                                    cfg.area_aware_edge_scroll,
                                    cfg.area_aware_edge_scroll_mode,
                                    cfg.scroll_trigger_line_only_when_open,
                                    is_open,
                                    has_slots,
                                )
                            });
                    if active {
                        wheel_capture_zones.push((
                            mon.id.clone(),
                            edge,
                            area_scroll_shade_rect(mon, edge),
                        ));
                    }
                }
            }
        }

        let active_charms_or_hover = self
            .charms
            .values()
            .any(DirectedCharmsDetector::is_any_armed)
            || self.bumps.values().any(EdgeBumpStateMachine::is_armed)
            || self.active_hover.is_some();
        let should_track_bezel_moves = self.edge_triggers_enabled
            && (matches!(
                cfg.gesture_mode,
                GestureMode::EdgeBump | GestureMode::CornerSlide | GestureMode::EdgeHover
            ) || (any_open && cfg.area_aware_edge_scroll)
                || !wheel_capture_zones.is_empty());

        LL_MOUSE_STATE.with(|cell| {
            let mut st = cell.borrow_mut();
            st.monitors = monitors;
            st.corner_zone_px = cfg.corner_zone_px;
            st.should_track_bezel_moves = should_track_bezel_moves;
            st.active_charms_or_hover = active_charms_or_hover;
            st.wheel_capture_zones = wheel_capture_zones;
        });
    }

    fn sync_area_scroll_shades(&mut self, cursor: Option<POINT>) {
        let cfg = self.engine.config().clone();
        let monitors = self.overlays.borrow().monitors.clone();
        for mon in &monitors {
            for edge in ScreenEdge::ALL {
                let key = (mon.id.clone(), edge);
                let is_open = self.engine.is_edge_open(&mon.id, edge);
                let has_slots = self.engine.has_slots_on_edge(&mon.id, edge);
                let on_line = cursor
                    .is_some_and(|pt| is_in_area_scroll_line(mon, edge, (pt.x, pt.y)))
                    || self.cursor_on_line_edges.contains(&key);
                let visible = self.area_scrolls.get(&key).is_some_and(|c| {
                    c.is_shade_visible(
                        cfg.area_aware_edge_scroll,
                        cfg.area_aware_edge_scroll_mode,
                        cfg.scroll_trigger_line_only_when_open,
                        is_open,
                        has_slots,
                        on_line,
                    )
                });
                self.overlays
                    .borrow_mut()
                    .set_area_scroll_shade(&mon.id, edge, visible);
            }
        }
    }

    fn sync_overlay_handles_to_engine(&mut self) {
        let mut cur_pt = POINT::default();
        let has_cursor = unsafe { GetCursorPos(&mut cur_pt).is_ok() };
        let monitors = self.overlays.borrow().monitors.clone();
        for mon in &monitors {
            for edge in ScreenEdge::ALL {
                let key = (mon.id.clone(), edge);
                let now_open = self.engine.is_edge_open(&mon.id, edge);
                let was_open = self.prev_open_edges.contains(&key);
                let cursor_on_strip =
                    has_cursor && is_in_area_scroll_line(mon, edge, (cur_pt.x, cur_pt.y));
                if cursor_on_strip {
                    self.cursor_on_line_edges.insert(key.clone());
                } else {
                    self.cursor_on_line_edges.remove(&key);
                }
                let opened_via_edge = self.last_opened_via_edge.as_ref() == Some(&key);
                {
                    let ctrl = self.area_scrolls.entry(key.clone()).or_default();
                    if !was_open && now_open {
                        ctrl.on_drawer_opened(opened_via_edge || cursor_on_strip, cursor_on_strip);
                        self.prev_open_edges.insert(key);
                    } else if was_open && !now_open {
                        ctrl.on_drawer_closed();
                        self.prev_open_edges.remove(&key);
                    }
                }
                self.sync_strip_hit_test(&mon.id, edge);
            }
        }
        if !self.engine.has_any_open_slot() {
            self.charms
                .values_mut()
                .for_each(DirectedCharmsDetector::reset);
            self.bumps
                .values_mut()
                .for_each(EdgeBumpStateMachine::reset);
        }
        self.last_opened_via_edge = None;
        self.sync_area_scroll_shades(has_cursor.then_some(cur_pt));
        self.sync_open_stashed_hwnds();
        self.sync_ll_mouse_snapshot();
        let overlay_hwnds = {
            let ov = self.overlays.borrow();
            ov.ensure_overlays_topmost();
            ov.all_overlay_hwnds()
        };
        self.engine.set_overlay_windows(overlay_hwnds);
    }

    fn handle_hotkey(&mut self, id: i32) {
        match id {
            HOTKEY_STASH_LEFT => {
                let _ = self.engine.stash_foreground(ScreenEdge::Left, false);
            }
            HOTKEY_STASH_RIGHT => {
                let _ = self.engine.stash_foreground(ScreenEdge::Right, false);
            }
            HOTKEY_STASH_TOP => {
                let _ = self.engine.stash_foreground(ScreenEdge::Top, false);
            }
            HOTKEY_STASH_BOTTOM => {
                let _ = self.engine.stash_foreground(ScreenEdge::Bottom, false);
            }
            HOTKEY_TOGGLE_ACTIVE => {
                let _ = self.engine.toggle_active();
            }
            HOTKEY_GROUP_ACTIVE => {
                let edge = self
                    .engine
                    .status_snapshot(0)
                    .slots
                    .into_iter()
                    .find(|s| s.is_open)
                    .map(|s| s.edge)
                    .unwrap_or(ScreenEdge::Right);
                let _ = self.engine.stash_foreground(edge, true);
            }
            HOTKEY_UNSTASH_ACTIVE => {
                if let Some(open_slot) = self
                    .engine
                    .status_snapshot(0)
                    .slots
                    .into_iter()
                    .find(|s| s.is_open)
                {
                    let _ = self
                        .engine
                        .unstash_active(Some(&open_slot.monitor_id), open_slot.edge);
                }
            }
            _ => {}
        }
        self.sync_overlay_handles_to_engine();
        self.update_dismiss_timer();
    }

    fn is_cursor_on_hover_target(
        &self,
        mon_id: &str,
        edge: ScreenEdge,
        pt: (i32, i32),
        allow_strip: bool,
        allow_pill: bool,
    ) -> bool {
        let ov = self.overlays.borrow();
        let Some(mon) = ov.monitors.iter().find(|m| m.id == mon_id) else {
            return false;
        };
        if allow_pill
            && let Some(pill) = ov.pills.get(&(mon_id.to_string(), edge))
            && pill.visible
            && pill.rect.contains_inclusive(pt.0, pt.1)
        {
            return true;
        }
        if allow_strip {
            let strip_rect = trigger_strip_rect(mon, edge, 1);
            if strip_rect.contains_inclusive(pt.0, pt.1) {
                return true;
            }
        }
        false
    }

    fn arm_or_trigger_hover(
        &mut self,
        mon_id: String,
        edge: ScreenEdge,
        from_pill: bool,
        from_drag: bool,
    ) {
        if self.engine.is_edge_open(&mon_id, edge) || !self.engine.has_slots_on_edge(&mon_id, edge)
        {
            return;
        }
        let cfg = self.engine.config().clone();
        let now = self.clock.now_ms();
        if let Some((prev_mon, prev_edge, _, _)) = &self.active_hover
            && (prev_mon != &mon_id || *prev_edge != edge)
            && let Some(prev) = self.hovers.get_mut(&(prev_mon.clone(), *prev_edge))
        {
            prev.reset();
        }
        self.active_hover = Some((mon_id.clone(), edge, from_pill, from_drag));
        let hover = self
            .hovers
            .entry((mon_id.clone(), edge))
            .or_insert_with(|| EdgeHoverStateMachine::new(cfg.edge_dwell_ms));
        hover.dwell_ms = cfg.edge_dwell_ms;
        if hover.update(true, now) {
            let triggered = if from_drag {
                self.engine.on_drag_over_pill_trigger(&mon_id, edge)
            } else {
                self.engine.on_edge_trigger(&mon_id, edge)
            };
            if triggered {
                self.engine.mark_drawer_opened_at(now);
                if !from_drag {
                    self.last_opened_via_edge = Some((mon_id.clone(), edge));
                }
                self.sync_overlay_handles_to_engine();
                self.update_dismiss_timer();
            }
            unsafe {
                let _ = KillTimer(self.msg_hwnd, TIMER_EDGE_HOVER);
            }
        } else {
            unsafe {
                let _ = SetTimer(self.msg_hwnd, TIMER_EDGE_HOVER, 20, None);
            }
        }
    }

    fn handle_strip_mouse_move(&mut self, strip_hwnd: HWND, lparam: LPARAM) {
        let mut pt = POINT {
            x: (lparam.0 & 0xFFFF) as i16 as i32,
            y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
        };
        let mut has_pt = unsafe { ClientToScreen(strip_hwnd, &mut pt).as_bool() };
        if !has_pt {
            has_pt = unsafe { GetCursorPos(&mut pt).is_ok() };
        }
        if has_pt {
            self.handle_global_mouse_move(pt);
        }
    }

    fn handle_global_mouse_move(&mut self, pt: POINT) {
        if self.engine.has_open_floating_drawer() {
            let mouse_down = unsafe {
                ((GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0)
                    || ((GetAsyncKeyState(VK_RBUTTON.0 as i32) as u16 & 0x8000) != 0)
            };
            if mouse_down {
                bring_drag_image_to_top();
            }
        }

        let monitors = self.overlays.borrow().monitors.clone();
        let cfg = self.engine.config().clone();
        let now = self.clock.now_ms();

        // 1. Update area-aware 4px edge line enter/leave state and shade visibility
        let mut any_shade_state_changed = false;
        for mon in &monitors {
            for edge in ScreenEdge::ALL {
                let key = (mon.id.clone(), edge);
                let is_open = self.engine.is_edge_open(&mon.id, edge);
                let on_line = is_in_area_scroll_line(mon, edge, (pt.x, pt.y));
                let was_on_line = self.cursor_on_line_edges.contains(&key);
                let on_pill = self
                    .overlays
                    .borrow()
                    .pills
                    .get(&key)
                    .is_some_and(|p| p.visible && p.rect.contains_inclusive(pt.x, pt.y));
                let ctrl = self.area_scrolls.entry(key.clone()).or_default();
                if !was_on_line && on_line {
                    self.cursor_on_line_edges.insert(key.clone());
                    ctrl.on_strip_enter(is_open);
                    any_shade_state_changed = true;
                } else if was_on_line && !on_line {
                    self.cursor_on_line_edges.remove(&key);
                    ctrl.on_strip_leave(is_open && !on_pill);
                    any_shade_state_changed = true;
                }
            }
        }

        // 2. Gesture handling: EdgeBump, CornerSlide, and/or EdgeHover
        let mut any_triggered = false;
        let supports_charms = cfg.gesture_mode == GestureMode::CornerSlide
            || cfg.gesture_mode == GestureMode::EdgeBump;

        if supports_charms {
            for mon in &monitors {
                if !mon.rect.expanded(24).contains_inclusive(pt.x, pt.y) {
                    if let Some(det) = self.charms.get_mut(&mon.id) {
                        det.reset();
                    }
                    continue;
                }
                let det = self.charms.entry(mon.id.clone()).or_insert_with(|| {
                    DirectedCharmsDetector::with_angle(
                        cfg.corner_zone_px,
                        cfg.slide_distance_px,
                        cfg.corner_timeout_ms,
                        cfg.churn_diagonal_angle_deg,
                    )
                });
                det.sync_params_dpi(
                    cfg.corner_zone_px,
                    cfg.slide_distance_px,
                    cfg.corner_timeout_ms,
                    cfg.churn_diagonal_angle_deg,
                    mon.dpi,
                );
                let fired = det.update(mon, (pt.x, pt.y), now);
                for zone in fired {
                    if !self.engine.has_churn_target(&mon.id, zone) {
                        continue;
                    }
                    if let Some((opened_mon, opened_edge)) =
                        self.engine.on_churn_zone_trigger(&mon.id, zone)
                    {
                        self.engine.mark_drawer_opened_at(now);
                        self.last_opened_via_edge = Some((opened_mon.clone(), opened_edge));
                        if let Some(bump) = self.bumps.get_mut(&(opened_mon, opened_edge)) {
                            bump.reset();
                        }
                        any_triggered = true;
                    }
                }
            }
        }

        if cfg.gesture_mode == GestureMode::EdgeBump {
            for mon in &monitors {
                for edge in ScreenEdge::ALL {
                    if !self.engine.has_slots_on_edge(&mon.id, edge) {
                        if let Some(bump) = self.bumps.get_mut(&(mon.id.clone(), edge)) {
                            bump.reset();
                        }
                        continue;
                    }
                    let key = (mon.id.clone(), edge);
                    let bump = self
                        .bumps
                        .entry(key)
                        .or_insert_with(|| EdgeBumpStateMachine::new(cfg.edge_bump_dwell_ms));
                    bump.dwell_ms = cfg.edge_bump_dwell_ms;
                    if bump.update(mon, edge, (pt.x, pt.y), now)
                        && self.engine.on_edge_trigger(&mon.id, edge)
                    {
                        self.engine.mark_drawer_opened_at(now);
                        self.last_opened_via_edge = Some((mon.id.clone(), edge));
                        any_triggered = true;
                    }
                }
            }
            let any_armed = self.bumps.values().any(EdgeBumpStateMachine::is_armed)
                || self
                    .charms
                    .values()
                    .any(DirectedCharmsDetector::is_any_armed);
            unsafe {
                if any_armed {
                    let _ = SetTimer(self.msg_hwnd, TIMER_CHARMS_POLL, 16, None);
                } else {
                    let _ = KillTimer(self.msg_hwnd, TIMER_CHARMS_POLL);
                }
            }
        } else if cfg.gesture_mode == GestureMode::CornerSlide {
            let any_armed = self
                .charms
                .values()
                .any(DirectedCharmsDetector::is_any_armed);
            unsafe {
                if any_armed {
                    let _ = SetTimer(self.msg_hwnd, TIMER_CHARMS_POLL, 16, None);
                } else {
                    let _ = KillTimer(self.msg_hwnd, TIMER_CHARMS_POLL);
                }
            }
        } else if allows_strip_hover(cfg.gesture_mode) {
            // 3. EdgeHover via global mouse observer (works cleanly across all apps/taskbar)
            let mut hovered_target: Option<(String, ScreenEdge)> = None;
            for mon in &monitors {
                for edge in ScreenEdge::ALL {
                    if self.engine.has_slots_on_edge(&mon.id, edge)
                        && trigger_strip_rect(mon, edge, 2).contains_inclusive(pt.x, pt.y)
                    {
                        hovered_target = Some((mon.id.clone(), edge));
                        break;
                    }
                }
                if hovered_target.is_some() {
                    break;
                }
            }
            if let Some((mon_id, edge)) = hovered_target {
                self.arm_or_trigger_hover(mon_id, edge, false, false);
            } else if let Some((prev_mon, prev_edge, false, false)) = self.active_hover.clone() {
                self.handle_edge_pointer_leave(&prev_mon, prev_edge);
            }
        }

        if any_triggered {
            self.sync_overlay_handles_to_engine();
            self.update_dismiss_timer();
        } else if any_shade_state_changed {
            self.sync_area_scroll_shades(Some(pt));
            self.sync_ll_mouse_snapshot();
        } else {
            self.sync_ll_mouse_snapshot();
        }
    }

    fn handle_edge_wheel_for(
        &mut self,
        mon_id: &str,
        edge: ScreenEdge,
        pt: POINT,
        delta: i16,
    ) -> bool {
        let cfg = self.engine.config().clone();
        let is_open = self.engine.is_edge_open(mon_id, edge);
        let slot_count = self.overlays.borrow().slot_count_for(mon_id, edge);
        let cursor_on_strip = self
            .overlays
            .borrow()
            .monitors
            .iter()
            .find(|m| m.id == mon_id)
            .is_some_and(|m| is_in_area_scroll_line(m, edge, (pt.x, pt.y)));
        let decision = {
            let ctrl = self
                .area_scrolls
                .entry((mon_id.to_string(), edge))
                .or_default();
            ctrl.on_strip_wheel(
                cfg.area_aware_edge_scroll,
                cfg.area_aware_edge_scroll_mode,
                cfg.scroll_trigger_line_only_when_open,
                is_open,
                cursor_on_strip,
                slot_count > 0,
            )
        };
        let step = if delta > 0 { -1 } else { 1 };
        match decision {
            StripScrollDecision::CycleAndOpenIfClosed => {
                self.last_opened_via_edge = Some((mon_id.to_string(), edge));
                let _ = self
                    .engine
                    .cycle_edge_and_open_if_closed(Some(mon_id), edge, step);
                self.sync_overlay_handles_to_engine();
                self.update_dismiss_timer();
                true
            }
            StripScrollDecision::CycleWindow => {
                let _ = self.engine.cycle_edge(Some(mon_id), edge, step);
                self.sync_overlay_handles_to_engine();
                self.update_dismiss_timer();
                true
            }
            StripScrollDecision::PassToWindow => {
                self.sync_strip_hit_test(mon_id, edge);
                self.sync_area_scroll_shades(Some(pt));
                self.sync_ll_mouse_snapshot();
                false
            }
        }
    }

    fn handle_edge_wheel_event(&mut self, wparam: WPARAM, lparam: LPARAM) {
        if self.drag_hover_pill.is_some() {
            return;
        }
        let delta = (wparam.0 as u16) as i16;
        let idx = (lparam.0 >> 4) as usize;
        let edge_opt = code_to_edge((lparam.0 & 0xF) as usize);
        let mon_id_opt = LL_MOUSE_STATE.with(|cell| {
            let st = cell.borrow();
            st.wheel_capture_zones
                .get(idx)
                .filter(|(_, e, _)| Some(*e) == edge_opt)
                .map(|(m, _, _)| m.clone())
                .or_else(|| {
                    edge_opt.and_then(|edge| {
                        st.wheel_capture_zones
                            .iter()
                            .find(|(_, e, _)| *e == edge)
                            .map(|(m, _, _)| m.clone())
                    })
                })
        });
        let mut pt = POINT::default();
        if unsafe { GetCursorPos(&mut pt).is_ok() }
            && let (Some(mon_id), Some(edge)) = (mon_id_opt, edge_opt)
        {
            let _ = self.handle_edge_wheel_for(&mon_id, edge, pt, delta);
        }
    }

    fn handle_strip_mouse_wheel(
        &mut self,
        strip_hwnd: HWND,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> bool {
        let Some((mon_id, edge)) = self.overlays.borrow().find_strip_by_hwnd(strip_hwnd) else {
            return false;
        };
        let wheel_pt = if lparam.0 != 0 {
            Some(POINT {
                x: (lparam.0 & 0xFFFF) as i16 as i32,
                y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
            })
        } else {
            let mut pt = POINT::default();
            unsafe { GetCursorPos(&mut pt).is_ok() }.then_some(pt)
        };
        let Some(pt) = wheel_pt else {
            return false;
        };
        let delta = ((wparam.0 >> 16) as u16) as i16;
        self.handle_edge_wheel_for(&mon_id, edge, pt, delta)
    }

    fn handle_pill_mouse_move(&mut self, pill_hwnd: HWND) {
        let Some((mon_id, edge)) = self.overlays.borrow().find_pill_by_hwnd(pill_hwnd) else {
            return;
        };
        let has_attention = self
            .overlays
            .borrow()
            .pills
            .get(&(mon_id.clone(), edge))
            .is_some_and(|p| !p.is_open && p.attention_count > 0);
        if has_attention || allows_pill_hover(self.engine.config().gesture_mode) {
            self.arm_or_trigger_hover(mon_id, edge, true, false);
        }
    }

    fn handle_pill_mouse_click(&mut self, pill_hwnd: HWND, lparam: LPARAM) {
        let info = {
            let ov = self.overlays.borrow();
            ov.find_pill_by_hwnd(pill_hwnd).and_then(|(mon_id, edge)| {
                ov.pills
                    .get(&(mon_id.clone(), edge))
                    .map(|p| (mon_id, edge, ov.pill_style, p.rect, p.slot_count))
            })
        };
        let Some((mon_id, edge, style, rect, slot_count)) = info else {
            return;
        };
        let attention_target = {
            let ov = self.overlays.borrow();
            ov.pills.get(&(mon_id.clone(), edge)).and_then(|p| {
                if !p.is_open && p.attention_count > 0 {
                    p.needs_attention.iter().position(|&a| a)
                } else {
                    None
                }
            })
        };
        if attention_target.is_none() && !allows_pill_click(self.engine.config().gesture_mode) {
            return;
        }
        let dpi = unsafe {
            let d = GetDpiForWindow(pill_hwnd);
            if d == 0 { 96 } else { d }
        };
        let cx = (lparam.0 & 0xFFFF) as i16 as i32;
        let cy = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
        if let Some(target_idx) = attention_target {
            let _ = self
                .engine
                .select_and_open_slot(Some(&mon_id), edge, target_idx, true);
        } else if let Some(idx) =
            hit_test_pill_slot_index(rect, edge, style, slot_count, dpi, (cx, cy))
        {
            let _ = self
                .engine
                .select_and_open_slot(Some(&mon_id), edge, idx, true);
        } else {
            let _ = self.engine.toggle_edge(Some(&mon_id), edge);
        }
        self.sync_overlay_handles_to_engine();
        self.update_dismiss_timer();
    }

    fn handle_header_mouse_move(&mut self, header_hwnd: HWND, lparam: LPARAM) {
        let mut ov = self.overlays.borrow_mut();
        let Some(key) = ov.find_header_by_hwnd(header_hwnd) else {
            return;
        };
        if let Some(h) = ov.headers.get_mut(&key) {
            let next = hit_test_header_button(h.rect, lparam);
            if h.hovered_button != next {
                h.hovered_button = next;
                OverlayWindowState::render_header_meta(h);
            }
        }
    }

    fn handle_header_mouse_leave(&mut self, header_hwnd: HWND) {
        let mut ov = self.overlays.borrow_mut();
        let Some(key) = ov.find_header_by_hwnd(header_hwnd) else {
            return;
        };
        if let Some(h) = ov.headers.get_mut(&key)
            && h.hovered_button.is_some()
        {
            h.hovered_button = None;
            OverlayWindowState::render_header_meta(h);
        }
    }

    fn handle_header_lbutton_up(&mut self, header_hwnd: HWND, lparam: LPARAM) {
        let hit = {
            let ov = self.overlays.borrow();
            ov.find_header_by_hwnd(header_hwnd).and_then(|key| {
                ov.headers
                    .get(&key)
                    .map(|h| (key.0, key.1, hit_test_header_button(h.rect, lparam)))
            })
        };
        let Some((mon_id, edge, btn)) = hit else {
            return;
        };
        match btn {
            Some(HeaderButtonKind::Close) => {
                let shift_held =
                    unsafe { (GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0 };
                if shift_held {
                    let _ = self.engine.close_slot_app(Some(&mon_id), edge);
                } else {
                    let _ = self.engine.close_slot_drawer(Some(&mon_id), edge);
                }
            }
            Some(HeaderButtonKind::Unstash) => {
                let _ = self.engine.unstash_active(Some(&mon_id), edge);
            }
            Some(HeaderButtonKind::Pin) => {
                let _ = self.engine.toggle_pin_active(Some(&mon_id), edge);
            }
            None => {}
        }
        self.sync_overlay_handles_to_engine();
        self.update_dismiss_timer();
    }

    fn handle_header_rbutton_up(&mut self, header_hwnd: HWND, lparam: LPARAM) {
        let hit = {
            let ov = self.overlays.borrow();
            ov.find_header_by_hwnd(header_hwnd).and_then(|key| {
                ov.headers
                    .get(&key)
                    .map(|h| (key.0, key.1, hit_test_header_button(h.rect, lparam)))
            })
        };
        let Some((mon_id, edge, btn)) = hit else {
            return;
        };
        match btn {
            Some(HeaderButtonKind::Close) => {
                let _ = self.engine.close_slot_app(Some(&mon_id), edge);
            }
            Some(HeaderButtonKind::Unstash) => {
                let _ = self.engine.unstash_active(Some(&mon_id), edge);
            }
            Some(HeaderButtonKind::Pin) | None => {
                self.show_header_pin_menu(&mon_id, edge);
            }
        }
        self.sync_overlay_handles_to_engine();
        self.update_dismiss_timer();
    }

    fn handle_ole_drag_enter(&mut self, pill_hwnd: HWND) {
        let Some((mon_id, edge)) = self.overlays.borrow().find_pill_by_hwnd(pill_hwnd) else {
            return;
        };
        self.drag_hover_pill = Some((mon_id.clone(), edge));
        self.drag_open_active = true;
        self.engine.set_drag_active(true);
        self.overlays.borrow_mut().set_drag_transparent(true);
        install_drag_wheel_hook();
        self.arm_or_trigger_hover(mon_id, edge, true, true);
        self.update_dismiss_timer();
    }

    fn handle_ole_drag_over(&mut self, pill_hwnd: HWND) {
        let Some((mon_id, edge)) = self.overlays.borrow().find_pill_by_hwnd(pill_hwnd) else {
            return;
        };
        if self.drag_hover_pill.as_ref() != Some(&(mon_id.clone(), edge)) {
            self.handle_ole_drag_enter(pill_hwnd);
        }
    }

    fn handle_ole_drag_leave(&mut self, pill_hwnd: HWND) {
        let left_key = self.overlays.borrow().find_pill_by_hwnd(pill_hwnd);
        if left_key.as_ref() == self.drag_hover_pill.as_ref() || left_key.is_none() {
            self.drag_hover_pill = None;
            uninstall_drag_wheel_hook();
        }
        if let Some((mon_id, edge)) = left_key
            && !self.engine.is_edge_open(&mon_id, edge)
        {
            if let Some(h) = self.hovers.get_mut(&(mon_id.clone(), edge)) {
                h.reset();
            }
            if self
                .active_hover
                .as_ref()
                .is_some_and(|(m, e, _, _)| m == &mon_id && *e == edge)
            {
                self.active_hover = None;
                unsafe {
                    let _ = KillTimer(self.msg_hwnd, TIMER_EDGE_HOVER);
                }
            }
        }
        self.update_dismiss_timer();
    }

    fn handle_ole_drag_wheel(&mut self, delta: i16) {
        let Some((mon_id, edge)) = self.drag_hover_pill.clone() else {
            return;
        };
        let step = if delta > 0 { -1 } else { 1 };
        if self
            .engine
            .cycle_edge_and_open_if_closed(Some(&mon_id), edge, step)
            .is_ok()
        {
            self.sync_overlay_handles_to_engine();
            self.update_dismiss_timer();
        }
    }

    fn finish_drag_session(&mut self) {
        uninstall_drag_wheel_hook();
        self.drag_hover_pill = None;
        self.drag_open_active = false;
        self.engine.set_drag_active(false);
        self.overlays.borrow_mut().set_drag_transparent(false);
    }

    fn handle_edge_pointer_leave(&mut self, mon_id: &str, edge: ScreenEdge) {
        let cfg = self.engine.config().clone();
        let mut pt = POINT::default();
        if unsafe { GetCursorPos(&mut pt).is_ok() } {
            let keep = self.is_cursor_on_hover_target(
                mon_id,
                edge,
                (pt.x, pt.y),
                allows_strip_hover(cfg.gesture_mode),
                allows_pill_hover(cfg.gesture_mode),
            );
            if keep {
                return;
            }
        }
        if let Some(hover) = self.hovers.get_mut(&(mon_id.to_string(), edge)) {
            hover.update(false, self.clock.now_ms());
        }
        if self
            .active_hover
            .as_ref()
            .is_some_and(|(m, e, _, _)| m == mon_id && *e == edge)
        {
            self.active_hover = None;
            unsafe {
                let _ = KillTimer(self.msg_hwnd, TIMER_EDGE_HOVER);
            }
        }
    }

    fn handle_strip_mouse_leave(&mut self, strip_hwnd: HWND) {
        let hit = self.overlays.borrow().find_strip_by_hwnd(strip_hwnd);
        if let Some((mon_id, edge)) = hit {
            let mut pt = POINT::default();
            let has_pt = unsafe { GetCursorPos(&mut pt).is_ok() };
            let (on_strip, on_pill) = if has_pt {
                let ov = self.overlays.borrow();
                let on_strip = ov
                    .monitors
                    .iter()
                    .find(|m| m.id == mon_id)
                    .is_some_and(|m| trigger_strip_rect(m, edge, 1).contains_inclusive(pt.x, pt.y));
                let on_pill = ov
                    .pills
                    .get(&(mon_id.clone(), edge))
                    .is_some_and(|p| p.visible && p.rect.contains_inclusive(pt.x, pt.y));
                (on_strip, on_pill)
            } else {
                (false, false)
            };
            if !on_strip {
                let is_open = self.engine.is_edge_open(&mon_id, edge);
                let into_drawer_body = is_open && !on_pill;
                self.area_scrolls
                    .entry((mon_id.clone(), edge))
                    .or_default()
                    .on_strip_leave(into_drawer_body);
                self.sync_strip_hit_test(&mon_id, edge);
            }
            self.handle_edge_pointer_leave(&mon_id, edge);
        }
    }

    fn handle_pill_mouse_leave(&mut self, pill_hwnd: HWND) {
        let hit = self.overlays.borrow().find_pill_by_hwnd(pill_hwnd);
        if let Some((mon_id, edge)) = hit {
            self.handle_edge_pointer_leave(&mon_id, edge);
        }
    }

    fn tick_edge_hover_timer(&mut self) {
        let Some((mon_id, edge, from_pill, from_drag)) = self.active_hover.clone() else {
            unsafe {
                let _ = KillTimer(self.msg_hwnd, TIMER_EDGE_HOVER);
            }
            return;
        };
        if self.engine.is_edge_open(&mon_id, edge) || !self.engine.has_slots_on_edge(&mon_id, edge)
        {
            if let Some(h) = self.hovers.get_mut(&(mon_id, edge)) {
                h.reset();
            }
            self.active_hover = None;
            unsafe {
                let _ = KillTimer(self.msg_hwnd, TIMER_EDGE_HOVER);
            }
            return;
        }
        let cfg = self.engine.config().clone();
        let allow_strip = allows_strip_hover(cfg.gesture_mode);
        let allow_pill = allows_pill_hover(cfg.gesture_mode);
        let mut pt = POINT::default();
        let drag_inside =
            from_drag && self.drag_hover_pill.as_ref() == Some(&(mon_id.clone(), edge));
        let inside = drag_inside
            || (unsafe { GetCursorPos(&mut pt).is_ok() }
                && self.is_cursor_on_hover_target(
                    &mon_id,
                    edge,
                    (pt.x, pt.y),
                    allow_strip,
                    allow_pill || from_pill,
                ));

        let now = self.clock.now_ms();
        let hover = self
            .hovers
            .entry((mon_id.clone(), edge))
            .or_insert_with(|| EdgeHoverStateMachine::new(cfg.edge_dwell_ms));
        hover.dwell_ms = cfg.edge_dwell_ms;

        if !inside {
            hover.update(false, now);
            self.active_hover = None;
            unsafe {
                let _ = KillTimer(self.msg_hwnd, TIMER_EDGE_HOVER);
            }
            return;
        }

        if hover.update(true, now) {
            let triggered = if from_drag {
                self.engine.on_drag_over_pill_trigger(&mon_id, edge)
            } else {
                self.engine.on_edge_trigger(&mon_id, edge)
            };
            if triggered {
                self.engine.mark_drawer_opened_at(now);
                if !from_drag {
                    self.last_opened_via_edge = Some((mon_id.clone(), edge));
                }
                self.sync_overlay_handles_to_engine();
                self.update_dismiss_timer();
            }
            unsafe {
                let _ = KillTimer(self.msg_hwnd, TIMER_EDGE_HOVER);
            }
        }
    }

    fn tick_charms_timer(&mut self) {
        let cfg = self.engine.config().clone();
        if cfg.gesture_mode != GestureMode::CornerSlide && cfg.gesture_mode != GestureMode::EdgeBump
        {
            self.charms.clear();
            self.bumps.clear();
            unsafe {
                let _ = KillTimer(self.msg_hwnd, TIMER_CHARMS_POLL);
            }
            return;
        }
        let mut pt = POINT::default();
        if unsafe { GetCursorPos(&mut pt).is_ok() } {
            self.handle_global_mouse_move(pt);
        }
    }

    fn active_edge_dock_mode(&self, mon_id: &str, edge: ScreenEdge) -> DockMode {
        self.engine
            .status_snapshot(0)
            .slots
            .into_iter()
            .find(|s| s.monitor_id == mon_id && s.edge == edge && s.is_active_in_deck)
            .map(|s| s.dock_mode)
            .unwrap_or(DockMode::FloatingOverlay)
    }

    fn show_tray_menu(&mut self) {
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
            let _ = SetForegroundWindow(self.msg_hwnd);
            if let Ok(menu) = CreatePopupMenu() {
                let current_mode = self.engine.config().gesture_mode;
                let mode_text = if current_mode == GestureMode::EdgeBump {
                    "Trigger: Edge Bump (Switch to Corner Slide)"
                } else if current_mode == GestureMode::CornerSlide {
                    "Trigger: Corner Slide (Switch to Edge Hover)"
                } else {
                    "Trigger: Edge Hover (Switch to Edge Bump)"
                };
                append_menu_item(menu, TRAY_CMD_SETTINGS, "Settings...", false);
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                append_menu_item(menu, TRAY_CMD_TOGGLE_GESTURE_MODE, mode_text, false);
                append_menu_item(menu, TRAY_CMD_UNSTASH_ALL, "Unstash All Windows", false);
                append_menu_item(menu, TRAY_CMD_RELOAD, "Reload Config", false);
                append_menu_item(menu, TRAY_CMD_EXIT, "Exit EdgeStash", false);

                let cmd = TrackPopupMenu(
                    menu,
                    TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RETURNCMD,
                    pt.x,
                    pt.y,
                    0,
                    self.msg_hwnd,
                    None,
                );
                let _ = DestroyMenu(menu);

                match cmd.0 as u32 {
                    TRAY_CMD_SETTINGS => launch_settings_ui(),
                    TRAY_CMD_TOGGLE_GESTURE_MODE => {
                        let mut next_cfg = self.engine.config().clone();
                        next_cfg.gesture_mode = if next_cfg.gesture_mode == GestureMode::EdgeBump {
                            GestureMode::CornerSlide
                        } else if next_cfg.gesture_mode == GestureMode::CornerSlide {
                            GestureMode::EdgeHover
                        } else {
                            GestureMode::EdgeBump
                        };
                        self.engine.update_config(next_cfg.clone());
                        self.charms.clear();
                        self.bumps.clear();
                        self.hovers.clear();
                        self.active_hover = None;
                        let _ = next_cfg.save_to_at(&self.config_path);
                    }
                    TRAY_CMD_UNSTASH_ALL => {
                        self.engine.unstash_all();
                        self.sync_overlay_handles_to_engine();
                        self.update_dismiss_timer();
                    }
                    TRAY_CMD_RELOAD => {
                        let _ = self.reload_config();
                    }
                    TRAY_CMD_EXIT => self.shutdown(),
                    _ => {}
                }
                if !self.should_quit {
                    self.sync_overlay_handles_to_engine();
                }
            }
        }
    }

    fn show_header_pin_menu(&mut self, mon_id: &str, edge: ScreenEdge) {
        let current = self.active_edge_dock_mode(mon_id, edge);
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
            let _ = SetForegroundWindow(self.msg_hwnd);
            if let Ok(menu) = CreatePopupMenu() {
                append_menu_item(
                    menu,
                    HEADER_CMD_PIN_FLOATING,
                    "Permanent Floating Pin (Stay Open)",
                    current == DockMode::FloatingPinned,
                );
                append_menu_item(
                    menu,
                    HEADER_CMD_PIN_DOCK,
                    "Direct Windows Dock (Reserve Space)",
                    current == DockMode::NativeWorkAreaDock,
                );
                append_menu_item(
                    menu,
                    HEADER_CMD_UNPIN,
                    "Auto-Hide (Unpin)",
                    current == DockMode::FloatingOverlay,
                );

                let cmd = TrackPopupMenu(
                    menu,
                    TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RETURNCMD,
                    pt.x,
                    pt.y,
                    0,
                    self.msg_hwnd,
                    None,
                );
                let _ = DestroyMenu(menu);

                match cmd.0 as u32 {
                    HEADER_CMD_PIN_FLOATING => {
                        let _ =
                            self.engine
                                .set_dock_mode(Some(mon_id), edge, DockMode::FloatingPinned);
                    }
                    HEADER_CMD_PIN_DOCK => {
                        let _ = self.engine.set_dock_mode(
                            Some(mon_id),
                            edge,
                            DockMode::NativeWorkAreaDock,
                        );
                    }
                    HEADER_CMD_UNPIN => {
                        let _ = self.engine.set_dock_mode(
                            Some(mon_id),
                            edge,
                            DockMode::FloatingOverlay,
                        );
                    }
                    _ => {}
                }
                self.update_dismiss_timer();
            }
        }
    }

    fn show_pill_menu(&mut self, mon_id: &str, edge: ScreenEdge) {
        let current = self.active_edge_dock_mode(mon_id, edge);
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
            let _ = SetForegroundWindow(self.msg_hwnd);
            if let Ok(menu) = CreatePopupMenu() {
                append_menu_item(menu, PILL_CMD_TOGGLE, "Toggle Drawer", false);
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                append_menu_item(
                    menu,
                    PILL_CMD_PIN_FLOATING,
                    "Permanent Floating Pin",
                    current == DockMode::FloatingPinned,
                );
                append_menu_item(
                    menu,
                    PILL_CMD_TOGGLE_DOCK,
                    "Direct Windows Dock (Reserve Space)",
                    current == DockMode::NativeWorkAreaDock,
                );
                append_menu_item(
                    menu,
                    PILL_CMD_UNPIN,
                    "Auto-Hide (Floating Overlay)",
                    current == DockMode::FloatingOverlay,
                );
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                append_menu_item(menu, PILL_CMD_UNSTASH, "Unstash Active Slot", false);
                append_menu_item(menu, PILL_CMD_SETTINGS, "Settings...", false);

                let cmd = TrackPopupMenu(
                    menu,
                    TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RETURNCMD,
                    pt.x,
                    pt.y,
                    0,
                    self.msg_hwnd,
                    None,
                );
                let _ = DestroyMenu(menu);

                match cmd.0 as u32 {
                    PILL_CMD_SETTINGS => launch_settings_ui(),
                    PILL_CMD_TOGGLE => {
                        let _ = self.engine.toggle_edge(Some(mon_id), edge);
                    }
                    PILL_CMD_PIN_FLOATING => {
                        let _ =
                            self.engine
                                .set_dock_mode(Some(mon_id), edge, DockMode::FloatingPinned);
                    }
                    PILL_CMD_TOGGLE_DOCK => {
                        let next_mode = if current == DockMode::NativeWorkAreaDock {
                            DockMode::FloatingOverlay
                        } else {
                            DockMode::NativeWorkAreaDock
                        };
                        let _ = self.engine.set_dock_mode(Some(mon_id), edge, next_mode);
                    }
                    PILL_CMD_UNPIN => {
                        let _ = self.engine.set_dock_mode(
                            Some(mon_id),
                            edge,
                            DockMode::FloatingOverlay,
                        );
                    }
                    PILL_CMD_UNSTASH => {
                        let _ = self.engine.unstash_active(Some(mon_id), edge);
                    }
                    _ => {}
                }
                self.update_dismiss_timer();
            }
        }
    }

    fn shutdown(&mut self) {
        if self.should_quit {
            return;
        }
        self.should_quit = true;
        uninstall_drag_wheel_hook();
        uninstall_global_mouse_hook();
        self.engine.unstash_all();
        self.overlays.borrow_mut().destroy_all_overlays();
        if self.hotkeys_enabled {
            for id in HOTKEY_STASH_LEFT..=HOTKEY_UNSTASH_ACTIVE {
                unsafe {
                    let _ = UnregisterHotKey(self.msg_hwnd, id);
                }
            }
        }
        if self.tray_active {
            remove_tray_icon(self.msg_hwnd);
        }
        unsafe {
            PostQuitMessage(0);
        }
    }
}

unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _id_event_thread: u32,
    _dwms_event_time: u32,
) {
    if id_object != OBJID_WINDOW.0 || hwnd.0.is_null() {
        return;
    }
    let msg_raw = GLOBAL_MSG_HWND.load(Ordering::Acquire);
    if msg_raw == 0 {
        return;
    }
    let target = HWND(msg_raw as *mut _);
    let msg_id = match event {
        EVENT_SYSTEM_FOREGROUND => WM_APP_WIN_FOREGROUND,
        EVENT_SYSTEM_MOVESIZESTART => WM_APP_WIN_MOVESIZESTART,
        EVENT_SYSTEM_MOVESIZEEND => WM_APP_WIN_MOVESIZEEND,
        EVENT_OBJECT_DESTROY => WM_APP_WIN_DESTROY,
        EVENT_SYSTEM_MINIMIZESTART => {
            let raw = hwnd.0 as isize;
            let tracked = OPEN_STASHED_HWNDS.with(|cell| cell.borrow().contains(&raw));
            if !tracked {
                return;
            }
            WM_APP_WIN_MINIMIZE
        }
        EVENT_OBJECT_LOCATIONCHANGE => {
            if id_child != CHILDID_SELF as i32 {
                return;
            }
            let raw = hwnd.0 as isize;
            let tracked = OPEN_STASHED_HWNDS.with(|cell| cell.borrow().contains(&raw));
            if !tracked {
                return;
            }
            WM_APP_WIN_LOCATIONCHANGE
        }
        _ => return,
    };
    unsafe {
        let _ = PostMessageW(target, msg_id, WPARAM(hwnd.0 as usize), LPARAM(0));
    }
}

unsafe extern "system" fn main_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut DaemonController;
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let ctrl = unsafe { &mut *ptr };

    match msg {
        WM_APP_IPC => {
            if lparam.0 != 0 {
                let payload = unsafe { Box::from_raw(lparam.0 as *mut IpcPayload) };
                let is_shutdown = matches!(payload.req, IpcRequest::Shutdown);
                let is_reload = matches!(payload.req, IpcRequest::ReloadConfig);
                if let IpcRequest::PreviewChurnVisual {
                    enabled,
                    zone,
                    corner_zone_px,
                    slide_distance_px,
                    diagonal_angle_deg,
                } = payload.req
                {
                    ctrl.overlays.borrow_mut().set_churn_previews(
                        enabled,
                        zone,
                        corner_zone_px,
                        slide_distance_px,
                        diagonal_angle_deg,
                    );
                }
                let mut resp = ctrl
                    .engine
                    .handle_ipc_request(payload.req, std::process::id());
                if is_reload && let Err(err) = ctrl.reload_config() {
                    resp = IpcResponse::Error {
                        code: IpcErrorCode::ConfigError,
                        message: err,
                    };
                }
                let _ = payload.reply_tx.send(resp);
                ctrl.sync_overlay_handles_to_engine();
                ctrl.update_dismiss_timer();
                if is_shutdown {
                    ctrl.shutdown();
                }
            } else {
                ctrl.shutdown();
            }
            LRESULT(0)
        }
        WM_APP_RELOAD_CONFIG => {
            let _ = ctrl.reload_config();
            LRESULT(0)
        }
        WM_APP_TRAY => {
            let mouse_msg = lparam.0 as u32;
            if mouse_msg == WM_LBUTTONDBLCLK {
                launch_settings_ui();
            } else if mouse_msg == WM_RBUTTONUP || mouse_msg == WM_LBUTTONUP {
                ctrl.show_tray_menu();
            }
            LRESULT(0)
        }
        WM_HOTKEY => {
            ctrl.handle_hotkey(wparam.0 as i32);
            LRESULT(0)
        }
        WM_APP_WIN_FOREGROUND => {
            let fg = WindowHandle(wparam.0 as isize);
            ctrl.engine
                .on_foreground_changed_at(fg, ctrl.clock.now_ms());
            ctrl.update_dismiss_timer();
            LRESULT(0)
        }
        WM_APP_WIN_MOVESIZESTART => {
            let h = WindowHandle(wparam.0 as isize);
            let cfg = ctrl.engine.config();
            if cfg.enable_drag_drop_zones {
                ctrl.drag_capture.edge_zone_px = cfg.drag_drop_zone_px;
                ctrl.drag_capture.corner_span_px = cfg.drag_corner_zone_px;
                ctrl.drag_capture.dwell_ms = cfg.drag_drop_dwell_ms as u64;
                ctrl.drag_capture.inset_px = cfg.drag_drop_inset_px;
                ctrl.drag_capture.require_modifier = cfg.drag_drop_require_modifier;
                ctrl.drag_capture.enabled_edges = [
                    cfg.drag_drop_left,
                    cfg.drag_drop_right,
                    cfg.drag_drop_top,
                    cfg.drag_drop_bottom,
                ];
                ctrl.drag_capture.begin_drag(h);
                unsafe {
                    let _ = SetTimer(ctrl.msg_hwnd, TIMER_DRAG_POLL, 16, None);
                }
            }
            LRESULT(0)
        }
        WM_APP_WIN_MOVESIZEEND => {
            let h = WindowHandle(wparam.0 as isize);
            unsafe {
                let _ = KillTimer(ctrl.msg_hwnd, TIMER_DRAG_POLL);
            }
            ctrl.overlays.borrow().emit_overlay_event_hide_preview();
            let raw = h.to_hwnd();
            let is_zoomed = unsafe { IsZoomed(raw).as_bool() };
            if is_zoomed && ctrl.engine.is_window_stashed_and_open(h) {
                let _ = ctrl
                    .engine
                    .unstash_window_with_state(h, UnstashTargetState::Maximize);
                ctrl.sync_overlay_handles_to_engine();
                ctrl.update_dismiss_timer();
            } else if ctrl.engine.config().enable_drag_drop_zones
                && let Some((drag_hwnd, target)) = ctrl.drag_capture.end_drag()
            {
                if ctrl
                    .engine
                    .config()
                    .is_drag_drop_edge_enabled(target.edge())
                {
                    match target {
                        DragDropTarget::Edge { edge, .. } => {
                            let _ = ctrl.engine.stash_window(drag_hwnd, edge, false);
                        }
                        DragDropTarget::Corner { zone, .. } => {
                            let _ = ctrl
                                .engine
                                .stash_window_and_bind_churn_zone(drag_hwnd, zone);
                            let _ = ctrl.engine.config().save_to_at(&ctrl.config_path);
                        }
                    }
                    ctrl.sync_overlay_handles_to_engine();
                    ctrl.update_dismiss_timer();
                } else {
                    ctrl.engine.on_window_moved_or_resized(h, false);
                    ctrl.sync_overlay_handles_to_engine();
                }
            } else {
                ctrl.engine.on_window_moved_or_resized(h, false);
                ctrl.sync_overlay_handles_to_engine();
            }
            LRESULT(0)
        }
        WM_APP_WIN_LOCATIONCHANGE => {
            let h = WindowHandle(wparam.0 as isize);
            let raw = h.to_hwnd();
            let is_zoomed = unsafe { IsZoomed(raw).as_bool() };
            if is_zoomed && ctrl.engine.is_window_stashed_and_open(h) {
                let _ = ctrl
                    .engine
                    .unstash_window_with_state(h, UnstashTargetState::Maximize);
                ctrl.sync_overlay_handles_to_engine();
                ctrl.update_dismiss_timer();
            } else {
                ctrl.engine.on_window_moved_or_resized(h, true);
            }
            LRESULT(0)
        }
        WM_APP_WIN_MINIMIZE => {
            let hwnd_raw = wparam.0 as isize;
            let target = WindowHandle(hwnd_raw);
            let raw = target.to_hwnd();
            let mut wr = RECT::default();
            let cursor_in_top = if unsafe { GetWindowRect(raw, &mut wr).is_ok() } {
                let mut pt = POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut pt);
                }
                pt.x >= wr.left - 4
                    && pt.x <= wr.right + 4
                    && pt.y >= wr.top - 10
                    && pt.y <= wr.top + 60
            } else {
                false
            };
            let lbutton_down = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) < 0 };
            let win_key_down = unsafe {
                GetAsyncKeyState(VK_LWIN.0 as i32) < 0 || GetAsyncKeyState(VK_RWIN.0 as i32) < 0
            };
            let clicked_hwnd = LAST_MINIMIZE_CLICK_HWND.load(Ordering::Acquire);
            let clicked_time = LAST_MINIMIZE_CLICK_TIME.load(Ordering::Acquire);
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let was_recent_click = clicked_hwnd == hwnd_raw
                && (now_ms.saturating_sub(clicked_time) <= 2000 || clicked_time == 0);
            let was_clicked =
                (was_recent_click || (cursor_in_top && lbutton_down)) && !win_key_down;
            if was_clicked && ctrl.engine.is_window_stashed(target) {
                LAST_MINIMIZE_CLICK_HWND.store(0, Ordering::Release);
                LAST_MINIMIZE_CLICK_TIME.store(0, Ordering::Release);
                let _ = ctrl
                    .engine
                    .unstash_window_with_state(target, UnstashTargetState::Minimize);
                ctrl.sync_overlay_handles_to_engine();
                ctrl.update_dismiss_timer();
            }
            LRESULT(0)
        }
        WM_APP_WIN_DESTROY => {
            let h = WindowHandle(wparam.0 as isize);
            ctrl.engine.on_window_destroyed(h);
            ctrl.sync_overlay_handles_to_engine();
            ctrl.update_dismiss_timer();
            LRESULT(0)
        }
        WM_APP_GLOBAL_MOUSEMOVE => {
            let pt = POINT {
                x: (wparam.0 as isize) as i32,
                y: lparam.0 as i32,
            };
            ctrl.handle_global_mouse_move(pt);
            LRESULT(0)
        }
        WM_APP_EDGE_WHEEL => {
            ctrl.handle_edge_wheel_event(wparam, lparam);
            LRESULT(0)
        }
        WM_APP_DRAG_ENTER => {
            ctrl.handle_ole_drag_enter(HWND(wparam.0 as *mut _));
            LRESULT(0)
        }
        WM_APP_DRAG_OVER => {
            ctrl.handle_ole_drag_over(HWND(wparam.0 as *mut _));
            LRESULT(0)
        }
        WM_APP_DRAG_LEAVE => {
            ctrl.handle_ole_drag_leave(HWND(wparam.0 as *mut _));
            LRESULT(0)
        }
        WM_APP_DRAG_DROP => {
            ctrl.finish_drag_session();
            ctrl.update_dismiss_timer();
            LRESULT(0)
        }
        WM_APP_DRAG_WHEEL => {
            let delta = (wparam.0 as u16) as i16;
            ctrl.handle_ole_drag_wheel(delta);
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            let monitors = Win32WindowSystem::new().enumerate_monitors();
            let enable_triggers = ctrl.edge_triggers_enabled;
            {
                let mut ov = ctrl.overlays.borrow_mut();
                ov.monitors = monitors;
                ov.rebuild_trigger_strips(enable_triggers);
            }
            ctrl.engine.on_monitors_changed();
            ctrl.sync_overlay_handles_to_engine();
            LRESULT(0)
        }
        WM_TIMER => {
            match wparam.0 {
                TIMER_DISMISS_CHECK => {
                    if ctrl.drag_open_active {
                        let l_down =
                            unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0 };
                        let r_down =
                            unsafe { (GetAsyncKeyState(VK_RBUTTON.0 as i32) as u16 & 0x8000) != 0 };
                        if !l_down && !r_down && ctrl.drag_hover_pill.is_none() {
                            ctrl.finish_drag_session();
                        }
                    }
                    let had_pending = ctrl.engine.has_pending_focus_restore();
                    let was_open = ctrl.engine.has_any_open_slot();
                    ctrl.engine.tick(&ctrl.clock);
                    if was_open != ctrl.engine.has_any_open_slot()
                        || (had_pending && !ctrl.engine.has_pending_focus_restore())
                    {
                        if was_open && !ctrl.engine.has_any_open_slot() {
                            ctrl.charms
                                .values_mut()
                                .for_each(DirectedCharmsDetector::reset);
                            ctrl.bumps
                                .values_mut()
                                .for_each(EdgeBumpStateMachine::reset);
                        }
                        ctrl.sync_overlay_handles_to_engine();
                        ctrl.update_dismiss_timer();
                    }
                }
                TIMER_EDGE_HOVER => {
                    ctrl.tick_edge_hover_timer();
                }
                TIMER_CHARMS_POLL => {
                    ctrl.tick_charms_timer();
                }
                TIMER_DRAG_POLL => {
                    let cfg = ctrl.engine.config();
                    if !cfg.enable_drag_drop_zones {
                        SharedOverlayBridge {
                            inner: ctrl.overlays.clone(),
                        }
                        .emit_overlay_event(PillOverlayEvent::HideDragPreview);
                        return LRESULT(0);
                    }
                    let mod_held = is_drag_modifier_held(cfg.drag_drop_modifier_key);
                    if cfg.drag_drop_require_modifier && !mod_held {
                        SharedOverlayBridge {
                            inner: ctrl.overlays.clone(),
                        }
                        .emit_overlay_event(PillOverlayEvent::HideDragPreview);
                        return LRESULT(0);
                    }
                    let mut pt = POINT::default();
                    if unsafe { GetCursorPos(&mut pt).is_ok() } {
                        let monitors = ctrl.overlays.borrow().monitors.clone();
                        let armed = ctrl.drag_capture.update_drag(
                            &monitors,
                            (pt.x, pt.y),
                            mod_held,
                            ctrl.clock.now_ms(),
                        );
                        let edge_zone = cfg.drag_drop_zone_px;
                        let corner_span = cfg.drag_corner_zone_px;
                        let effective_inset = if mod_held || cfg.drag_drop_require_modifier {
                            cfg.drag_drop_inset_px
                        } else {
                            0
                        };
                        let prox = edge_zone.max(corner_span) + 20;

                        let active_target =
                            armed.or_else(|| ctrl.drag_capture.hovering_target().cloned());

                        let near = if let Some(target) = &active_target {
                            monitors
                                .iter()
                                .find(|m| m.id == target.monitor_id())
                                .map(|m| (m, target.edge()))
                        } else {
                            detect_near_edge_ext(
                                &monitors,
                                prox,
                                effective_inset,
                                |e| cfg.is_drag_drop_edge_enabled(e),
                                (pt.x, pt.y),
                            )
                        };

                        if let Some((mon, edge)) = near {
                            let band_rect = compute_edge_band_rect_inset(
                                mon,
                                edge,
                                edge_zone,
                                corner_span,
                                effective_inset,
                            );
                            SharedOverlayBridge {
                                inner: ctrl.overlays.clone(),
                            }
                            .emit_overlay_event(
                                PillOverlayEvent::ShowDragPreview {
                                    monitor_id: mon.id.clone(),
                                    edge,
                                    band_rect,
                                    active_target,
                                    edge_zone_px: edge_zone,
                                    corner_span_px: corner_span,
                                },
                            );
                        } else {
                            SharedOverlayBridge {
                                inner: ctrl.overlays.clone(),
                            }
                            .emit_overlay_event(PillOverlayEvent::HideDragPreview);
                        }
                    }
                }
                TIMER_CLOSE_ANIM => {
                    let still_running = ctrl.overlays.borrow_mut().tick_close_animations();
                    if !still_running {
                        unsafe {
                            let _ = KillTimer(ctrl.msg_hwnd, TIMER_CLOSE_ANIM);
                        }
                    }
                }
                TIMER_DIM_FADE => {
                    let still_running = ctrl.overlays.borrow_mut().tick_dim_fade();
                    if !still_running {
                        unsafe {
                            let _ = KillTimer(ctrl.msg_hwnd, TIMER_DIM_FADE);
                        }
                    }
                }
                TIMER_ATTENTION_PULSE => {
                    let mut ov = ctrl.overlays.borrow_mut();
                    let has_any_attention =
                        ov.desired_pills.values().any(|p| p.attention_count > 0);
                    if has_any_attention {
                        let keys: Vec<(String, ScreenEdge)> = ov
                            .desired_pills
                            .iter()
                            .filter_map(|(k, v)| {
                                if v.attention_count > 0 {
                                    Some(k.clone())
                                } else {
                                    None
                                }
                            })
                            .collect();
                        for (mon_id, edge) in keys {
                            ov.sync_pill(&mon_id, edge);
                        }
                    } else {
                        unsafe {
                            let _ = KillTimer(ctrl.msg_hwnd, TIMER_ATTENTION_PULSE);
                        }
                    }
                }
                TIMER_TRAY_RETRY => {
                    if add_tray_icon(ctrl.msg_hwnd) || ctrl.tray_retries_remaining == 0 {
                        unsafe {
                            let _ = KillTimer(ctrl.msg_hwnd, TIMER_TRAY_RETRY);
                        }
                        ctrl.tray_retries_remaining = 0;
                        ctrl.tray_active = true;
                    } else {
                        ctrl.tray_retries_remaining = ctrl.tray_retries_remaining.saturating_sub(1);
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        msg if msg == WM_SHELLHOOK_MSG.load(Ordering::Relaxed) && msg != 0 => {
            let code = wparam.0 as u32;
            const HSHELL_FLASH: u32 = 0x8006;
            const HSHELL_RUDEAPPACTIVATED: u32 = 0x8004;
            if code == HSHELL_FLASH || code == HSHELL_RUDEAPPACTIVATED {
                let target_hwnd = HWND(lparam.0 as *mut _);
                let now = ctrl.clock.now_ms();
                if let Some((_mon, _edge, _slot)) = ctrl
                    .engine
                    .handle_window_attention(WindowHandle::from_hwnd(target_hwnd), now)
                {
                    unsafe {
                        let _ = SetTimer(ctrl.msg_hwnd, TIMER_ATTENTION_PULSE, 33, None);
                    }
                }
            }
            LRESULT(0)
        }
        msg if msg == WM_TASKBAR_CREATED_MSG.load(Ordering::Relaxed) && msg != 0 => {
            if ctrl.tray_active || ctrl.tray_retries_remaining > 0 {
                if !add_tray_icon(ctrl.msg_hwnd) {
                    ctrl.tray_retries_remaining = 5;
                    unsafe {
                        let _ = SetTimer(ctrl.msg_hwnd, TIMER_TRAY_RETRY, 500, None);
                    }
                } else {
                    ctrl.tray_active = true;
                }
            }
            ctrl.engine.reapply_taskbar_deletion();
            LRESULT(0)
        }
        WM_QUERYENDSESSION => LRESULT(1),
        WM_ENDSESSION => {
            if wparam.0 != 0 {
                ctrl.shutdown();
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            ctrl.shutdown();
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

impl OverlayWindowState {
    fn emit_overlay_event_hide_preview(&self) {
        if let Some((hwnd, _, _)) = &self.drag_preview {
            unsafe {
                let _ = ShowWindow(*hwnd, SW_HIDE);
            }
        }
    }
}

unsafe extern "system" fn strip_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = unsafe {
            &*(lparam.0 as *const windows::Win32::UI::WindowsAndMessaging::CREATESTRUCTW)
        };
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize) };
        return LRESULT(1);
    }
    let owner = HWND(unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut _);
    if !owner.0.is_null() {
        let ctrl_ptr = unsafe { GetWindowLongPtrW(owner, GWLP_USERDATA) } as *mut DaemonController;
        if !ctrl_ptr.is_null() {
            let ctrl = unsafe { &mut *ctrl_ptr };
            match msg {
                WM_MOUSEMOVE => {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    unsafe {
                        let _ = TrackMouseEvent(&mut tme);
                    }
                    ctrl.handle_strip_mouse_move(hwnd, lparam);
                    return LRESULT(0);
                }
                WM_MOUSELEAVE => {
                    ctrl.handle_strip_mouse_leave(hwnd);
                    return LRESULT(0);
                }
                WM_MOUSEWHEEL if ctrl.handle_strip_mouse_wheel(hwnd, wparam, lparam) => {
                    return LRESULT(0);
                }
                _ => {}
            }
        }
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

unsafe extern "system" fn overlay_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = unsafe {
            &*(lparam.0 as *const windows::Win32::UI::WindowsAndMessaging::CREATESTRUCTW)
        };
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize) };
        return LRESULT(1);
    }
    let owner = HWND(unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut _);
    if !owner.0.is_null() {
        let ctrl_ptr = unsafe { GetWindowLongPtrW(owner, GWLP_USERDATA) } as *mut DaemonController;
        if !ctrl_ptr.is_null() {
            let ctrl = unsafe { &mut *ctrl_ptr };
            match msg {
                WM_MOUSEMOVE => {
                    let (is_pill, is_header) = {
                        let ov = ctrl.overlays.borrow();
                        (
                            ov.find_pill_by_hwnd(hwnd).is_some(),
                            ov.find_header_by_hwnd(hwnd).is_some(),
                        )
                    };
                    if is_pill || is_header {
                        let mut tme = TRACKMOUSEEVENT {
                            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        unsafe {
                            let _ = TrackMouseEvent(&mut tme);
                        }
                        if is_pill {
                            ctrl.handle_pill_mouse_move(hwnd);
                        } else {
                            ctrl.handle_header_mouse_move(hwnd, lparam);
                        }
                        return LRESULT(0);
                    }
                }
                WM_MOUSELEAVE => {
                    let (is_pill, is_header) = {
                        let ov = ctrl.overlays.borrow();
                        (
                            ov.find_pill_by_hwnd(hwnd).is_some(),
                            ov.find_header_by_hwnd(hwnd).is_some(),
                        )
                    };
                    if is_pill {
                        ctrl.handle_pill_mouse_leave(hwnd);
                        return LRESULT(0);
                    }
                    if is_header {
                        ctrl.handle_header_mouse_leave(hwnd);
                        return LRESULT(0);
                    }
                }
                WM_LBUTTONUP => {
                    let (is_header, is_pill) = {
                        let ov = ctrl.overlays.borrow();
                        (
                            ov.find_header_by_hwnd(hwnd).is_some(),
                            ov.find_pill_by_hwnd(hwnd).is_some(),
                        )
                    };
                    if is_header {
                        ctrl.handle_header_lbutton_up(hwnd, lparam);
                        return LRESULT(0);
                    }
                    if is_pill {
                        ctrl.handle_pill_mouse_click(hwnd, lparam);
                        return LRESULT(0);
                    }
                }
                WM_LBUTTONDOWN => {
                    if ctrl.overlays.borrow().is_scrim_hwnd(hwnd) {
                        ctrl.engine.on_foreground_changed(WindowHandle::NULL);
                        ctrl.update_dismiss_timer();
                        return LRESULT(0);
                    }
                }
                WM_RBUTTONUP => {
                    let is_header = ctrl.overlays.borrow().find_header_by_hwnd(hwnd).is_some();
                    if is_header {
                        ctrl.handle_header_rbutton_up(hwnd, lparam);
                        return LRESULT(0);
                    }
                    let pill = ctrl.overlays.borrow().find_pill_by_hwnd(hwnd);
                    if let Some((mon_id, edge)) = pill {
                        ctrl.show_pill_menu(&mon_id, edge);
                        ctrl.sync_overlay_handles_to_engine();
                        return LRESULT(0);
                    }
                }
                WM_MOUSEWHEEL => {
                    let pill = ctrl.overlays.borrow().find_pill_by_hwnd(hwnd);
                    if let Some((mon_id, edge)) = pill {
                        let delta = ((wparam.0 >> 16) as u16) as i16;
                        let step = if delta > 0 { -1 } else { 1 };
                        let _ = ctrl.engine.cycle_edge(Some(&mon_id), edge, step);
                        ctrl.sync_overlay_handles_to_engine();
                        ctrl.update_dismiss_timer();
                        return LRESULT(0);
                    }
                }
                _ => {}
            }
        }
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn register_window_classes(hinst: HINSTANCE) -> windows::core::Result<()> {
    let main_cls = to_wide_null("EdgeStashMessageWindow");
    let strip_cls = to_wide_null("EdgeStashTriggerStrip");
    let overlay_cls = to_wide_null("EdgeStashD2DOverlay");

    type WndClassEntry<'a> = (
        &'a [u16],
        Option<unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT>,
    );
    let classes: [WndClassEntry<'_>; 3] = [
        (&main_cls, Some(main_wnd_proc)),
        (&strip_cls, Some(strip_wnd_proc)),
        (&overlay_cls, Some(overlay_wnd_proc)),
    ];

    let arrow = unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() };
    for (name, wndproc) in classes {
        let wc = WNDCLASSW {
            lpfnWndProc: wndproc,
            hInstance: hinst,
            hCursor: arrow,
            hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as *mut _),
            lpszClassName: PCWSTR(name.as_ptr()),
            ..Default::default()
        };
        unsafe {
            let _ = RegisterClassW(&wc);
        }
    }
    Ok(())
}

fn add_tray_icon(hwnd: HWND) -> bool {
    let mut nid = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_APP_TRAY,
        hIcon: unsafe { LoadIconW(None, IDI_APPLICATION).unwrap_or_default() },
        ..Default::default()
    };
    let tip = "EdgeStash Universal Drawer Daemon";
    for (i, ch) in tip.encode_utf16().take(nid.szTip.len() - 1).enumerate() {
        nid.szTip[i] = ch;
    }
    let added = unsafe { Shell_NotifyIconW(NIM_ADD, &nid).as_bool() };
    if !added {
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid).as_bool() }
    } else {
        true
    }
}

fn remove_tray_icon(hwnd: HWND) {
    let nid = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        ..Default::default()
    };
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

fn spawn_ipc_thread(
    mut server: PipeServer,
    msg_hwnd_raw: isize,
    shutdown_flag: Arc<AtomicBool>,
    ipc_finished: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while !shutdown_flag.load(Ordering::Acquire) {
            let mut saw_shutdown = false;
            let res = server.accept_one(|req| {
                if shutdown_flag.load(Ordering::Acquire) {
                    saw_shutdown = true;
                    return IpcResponse::Ok {
                        message: "Shutting down".to_string(),
                    };
                }
                saw_shutdown = matches!(req, IpcRequest::Shutdown);
                let (reply_tx, reply_rx) = mpsc::channel();
                let payload = Box::new(IpcPayload { req, reply_tx });
                let ptr = Box::into_raw(payload);
                let posted = unsafe {
                    PostMessageW(
                        HWND(msg_hwnd_raw as *mut _),
                        WM_APP_IPC,
                        WPARAM(0),
                        LPARAM(ptr as isize),
                    )
                };
                if posted.is_err() {
                    unsafe {
                        drop(Box::from_raw(ptr));
                    }
                    return IpcResponse::Error {
                        code: edgestash_ipc::protocol::IpcErrorCode::InternalError,
                        message: "Daemon message loop unavailable".to_string(),
                    };
                }
                reply_rx.recv().unwrap_or(IpcResponse::Error {
                    code: edgestash_ipc::protocol::IpcErrorCode::InternalError,
                    message: "Daemon did not reply".to_string(),
                })
            });

            if res.is_err() || saw_shutdown {
                break;
            }
        }
        ipc_finished.store(true, Ordering::Release);
    })
}

fn spawn_config_watcher_thread(
    config_path: PathBuf,
    msg_hwnd_raw: isize,
    shutdown_flag: Arc<AtomicBool>,
) {
    let Some(dir) = config_path.parent().map(Path::to_path_buf) else {
        return;
    };
    thread::spawn(move || {
        let dir_wide = to_wide_null(&dir.to_string_lossy());
        let handle = unsafe {
            CreateFileW(
                PCWSTR(dir_wide.as_ptr()),
                FILE_LIST_DIRECTORY.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                None,
            )
        };
        let Ok(dir_handle) = handle else {
            return;
        };
        let mut buf = [0u8; 4096];
        let mut returned = 0u32;
        while !shutdown_flag.load(Ordering::Acquire) {
            let res = unsafe {
                ReadDirectoryChangesW(
                    dir_handle,
                    buf.as_mut_ptr() as *mut _,
                    buf.len() as u32,
                    false,
                    FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_FILE_NAME,
                    Some(&mut returned),
                    None,
                    None,
                )
            };
            if res.is_err() || shutdown_flag.load(Ordering::Acquire) {
                break;
            }
            unsafe {
                let _ = PostMessageW(
                    HWND(msg_hwnd_raw as *mut _),
                    WM_APP_RELOAD_CONFIG,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
        unsafe {
            let _ = CloseHandle(dir_handle);
        }
    });
}

/// Runs the EdgeStash daemon with default production options.
pub fn run_daemon() -> Result<(), String> {
    run_daemon_with_options(DaemonLaunchOptions::default(), None)
}

/// Runs the EdgeStash daemon message loop with configurable launch options and readiness hook.
pub fn run_daemon_with_options(
    opts: DaemonLaunchOptions,
    ready_tx: Option<mpsc::Sender<()>>,
) -> Result<(), String> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let _mutex_guard = if let Some(mutex_name) = &opts.single_instance_mutex {
        let wide = to_wide_null(mutex_name);
        let handle = unsafe {
            CreateMutexW(None, true, PCWSTR(wide.as_ptr()))
                .map_err(|e| format!("failed creating mutex: {e}"))?
        };
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            let _ = unsafe { CloseHandle(handle) };
            return Err("EdgeStash daemon is already running".to_string());
        }
        Some(MutexGuard(handle))
    } else {
        None
    };

    // Declare ComGuard BEFORE engine so COM uninitializes AFTER all COM proxies drop!
    let _com = ComGuard::init();

    let _ = emergency_restore_from_manifest(&opts.manifest_path);
    install_safety_hooks(opts.manifest_path.clone());

    let cfg = EdgeStashConfig::load_or_create_at(&opts.config_path)
        .map_err(|e| format!("failed loading config {}: {e}", opts.config_path.display()))?;

    let hinst: HINSTANCE = unsafe { GetModuleHandleW(None) }
        .map_err(|e| format!("GetModuleHandleW failed: {e}"))?
        .into();
    register_window_classes(hinst).map_err(|e| format!("RegisterClassW failed: {e}"))?;

    let msg_cls = to_wide_null("EdgeStashMessageWindow");
    let msg_title = to_wide_null("EdgeStashMessageHost");
    let msg_hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            PCWSTR(msg_cls.as_ptr()),
            PCWSTR(msg_title.as_ptr()),
            WS_POPUP,
            -32000,
            -32000,
            1,
            1,
            None,
            None,
            hinst,
            None,
        )
    }
    .map_err(|e| format!("CreateWindowExW message host failed: {e}"))?;
    GLOBAL_MSG_HWND.store(msg_hwnd.0 as isize, Ordering::Release);
    let shell_msg = unsafe { RegisterWindowMessageW(w!("SHELLHOOK")) };
    if shell_msg != 0 {
        WM_SHELLHOOK_MSG.store(shell_msg, Ordering::Release);
        unsafe {
            let _ = RegisterShellHookWindow(msg_hwnd);
            let _ = ChangeWindowMessageFilterEx(msg_hwnd, shell_msg, MSGFLT_ALLOW, None);
        }
    }
    let taskbar_msg = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
    if taskbar_msg != 0 {
        WM_TASKBAR_CREATED_MSG.store(taskbar_msg, Ordering::Release);
        unsafe {
            let _ = ChangeWindowMessageFilterEx(msg_hwnd, taskbar_msg, MSGFLT_ALLOW, None);
        }
    }
    unsafe {
        let _ = ChangeWindowMessageFilterEx(msg_hwnd, WM_APP_TRAY, MSGFLT_ALLOW, None);
    }

    // Ensure message queue exists before background threads call PostMessageW
    unsafe {
        let mut msg = MSG::default();
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
    }

    let win_sys = Win32WindowSystem::new();
    let monitors = win_sys.enumerate_monitors();
    let vdesktop = Win32VirtualDesktop::new(msg_hwnd)
        .map_err(|e| format!("IVirtualDesktopManager failed: {e}"))?;
    let appbar = Win32AppBar::new(msg_hwnd);

    let overlays = Rc::new(RefCell::new(OverlayWindowState::new(
        msg_hwnd,
        cfg.edge_pill_style,
        cfg.auto_hide_pill,
        cfg.dim_fade_duration_ms,
        monitors,
    )));
    overlays
        .borrow_mut()
        .rebuild_trigger_strips(opts.enable_edge_triggers);

    let clock = SystemClock::new();
    let mut engine = EdgeStashEngine::new(
        win_sys,
        vdesktop,
        appbar,
        SharedOverlayBridge {
            inner: overlays.clone(),
        },
        &clock,
        cfg,
        opts.manifest_path.clone(),
    );
    engine.set_config_path(opts.config_path.clone());
    engine.set_overlay_windows(overlays.borrow().all_overlay_hwnds());

    let tray_active = if opts.enable_tray {
        add_tray_icon(msg_hwnd)
    } else {
        false
    };

    let mut controller = Box::new(DaemonController {
        engine,
        overlays,
        clock,
        msg_hwnd,
        config_path: opts.config_path.clone(),
        charms: HashMap::new(),
        bumps: HashMap::new(),
        hovers: HashMap::new(),
        area_scrolls: HashMap::new(),
        cursor_on_line_edges: HashSet::new(),
        prev_open_edges: HashSet::new(),
        last_opened_via_edge: None,
        active_hover: None,
        drag_hover_pill: None,
        drag_open_active: false,
        drag_capture: DragCaptureStateMachine::new(80, 350),
        hotkeys_enabled: opts.enable_hotkeys,
        edge_triggers_enabled: opts.enable_edge_triggers,
        tray_active,
        tray_retries_remaining: 0,
        should_quit: false,
    });
    controller.sync_hotkeys();
    controller.sync_overlay_handles_to_engine();

    let ctrl_raw = (&mut *controller as *mut DaemonController) as isize;
    unsafe {
        SetWindowLongPtrW(msg_hwnd, GWLP_USERDATA, ctrl_raw);
    }

    if opts.enable_edge_triggers {
        install_global_mouse_hook();
    }

    let mut hooks: Vec<HWINEVENTHOOK> = Vec::new();
    if opts.enable_win_events {
        let ranges = [
            (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
            (EVENT_SYSTEM_MOVESIZESTART, EVENT_SYSTEM_MOVESIZEEND),
            (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
            (EVENT_OBJECT_DESTROY, EVENT_OBJECT_DESTROY),
            (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE),
        ];
        for (min_ev, max_ev) in ranges {
            let h = unsafe {
                SetWinEventHook(
                    min_ev,
                    max_ev,
                    None,
                    Some(win_event_proc),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                )
            };
            if !h.is_invalid() {
                hooks.push(h);
            }
        }
    }

    let shutdown_flag = Arc::new(AtomicBool::new(false));
    let ipc_finished = Arc::new(AtomicBool::new(false));
    let pipe_server = PipeServer::bind(&opts.pipe_name)
        .map_err(|e| format!("failed binding IPC pipe {}: {e}", opts.pipe_name))?;
    let ipc_thread = spawn_ipc_thread(
        pipe_server,
        msg_hwnd.0 as isize,
        shutdown_flag.clone(),
        ipc_finished.clone(),
    );
    spawn_config_watcher_thread(
        opts.config_path.clone(),
        msg_hwnd.0 as isize,
        shutdown_flag.clone(),
    );

    if let Some(tx) = ready_tx {
        let _ = tx.send(());
    }

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    shutdown_flag.store(true, Ordering::Release);
    if !ipc_finished.load(Ordering::Acquire) {
        // Wake ConnectNamedPipe when shutdown came from Tray Exit / WM_ENDSESSION / Ctrl+C.
        let _ = PipeClient::new(&opts.pipe_name).send(&IpcRequest::Ping);
    }
    for h in hooks {
        unsafe {
            let _ = UnhookWinEvent(h);
        }
    }
    controller.shutdown();
    unsafe {
        let _ = DeregisterShellHookWindow(msg_hwnd);
        SetWindowLongPtrW(msg_hwnd, GWLP_USERDATA, 0);
        let _ = DestroyWindow(msg_hwnd);
    }
    GLOBAL_MSG_HWND.store(0, Ordering::Release);
    let _ = ipc_thread.join();
    Ok(())
}
