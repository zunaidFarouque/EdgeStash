use std::cell::RefCell;
use std::collections::HashMap;
use std::mem::size_of;

use edgestash_ipc::types::{AnimationStyle, Rect, ScreenEdge};
use windows::Win32::Foundation::{BOOL, CloseHandle, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MONITORINFOEXW, MonitorFromWindow, RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE,
    RDW_UPDATENOW, RedrawWindow,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Shell::{ITaskbarList, TaskbarList};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, FindWindowExW, GA_ROOT, GA_ROOTOWNER,
    GWL_EXSTYLE, GWL_STYLE, GWLP_USERDATA, GetAncestor, GetClassNameW, GetCursorPos,
    GetDesktopWindow, GetForegroundWindow, GetParent, GetShellWindow, GetTopWindow,
    GetWindowLongPtrW, GetWindowPlacement, GetWindowRect, GetWindowThreadProcessId, HWND_NOTOPMOST,
    HWND_TOPMOST, IsIconic, IsWindow, IsWindowVisible, IsZoomed, MONITORINFOF_PRIMARY,
    PostMessageW, RegisterClassExW, SC_MAXIMIZE, SC_MINIMIZE, SMTO_ABORTIFHUNG, SW_MAXIMIZE,
    SW_MINIMIZE, SW_RESTORE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOCOPYBITS, SWP_NOMOVE,
    SWP_NOSIZE, SWP_NOZORDER, SendMessageTimeoutW, SetWindowLongPtrW, SetWindowPlacement,
    SetWindowPos, ShowWindow, WINDOWPLACEMENT, WM_CLOSE, WM_GETTEXT, WM_SYSCOMMAND, WNDCLASSEXW,
    WS_CAPTION, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{GUID, PCWSTR, Result, w};

use crate::animator::{Win32TransitionTarget, animate_windows_transition};
use crate::appbar::register_or_update_native_dock;
use crate::dwm::{
    get_extended_frame_bounds, get_invisible_border_insets, is_window_cloaked, set_window_cloaked,
};
use crate::focus::{force_restore_foreground_window, has_active_popup_or_menu};
use crate::geometry::{visible_to_window_origin, visible_to_window_rect};
use crate::platform::{
    AppBarApi, MonitorInfo, UnstashTargetState, VirtualDesktopApi, WindowAccessError, WindowHandle,
    WindowSnapshot, WindowSystem, WindowTransition,
};
use crate::vdesktop::VirtualDesktopController;

const PROTECTED_CLASSES: &[&str] = &[
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Progman",
    "WorkerW",
    "DV2ControlHost",
    "Windows.UI.Core.CoreWindow",
    "MultitaskingViewFrame",
];

/// Queries the lowercase executable basename (e.g. `"spotify.exe"`) for `pid`.
pub fn query_process_basename_for_pid(pid: u32) -> String {
    if pid == 0 {
        return String::new();
    }
    let Ok(proc_handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
    else {
        return String::new();
    };
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(
            proc_handle,
            PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok()
    };
    let _ = unsafe { CloseHandle(proc_handle) };
    if !ok || len == 0 {
        return String::new();
    }
    let full = String::from_utf16_lossy(&buf[..len as usize]);
    std::path::Path::new(&full)
        .file_name()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_else(|| full.to_ascii_lowercase())
}

/// Returns `true` for desktop/shell/EdgeStash window classes that must never be stashed
/// or falsely treated as active fullscreen games/videos.
pub fn is_non_fullscreen_shell_or_internal_class(class_name: &str) -> bool {
    class_name.starts_with("EdgeStash") || PROTECTED_CLASSES.contains(&class_name)
}

/// Thin `unsafe` Win32/DWM/Taskbar adapter implementing [`WindowSystem`].
#[derive(Default)]
pub struct Win32WindowSystem {
    taskbar: RefCell<Option<ITaskbarList>>,
}

impl Win32WindowSystem {
    pub fn new() -> Self {
        Self::default()
    }

    fn with_taskbar<F>(&self, f: F)
    where
        F: FnOnce(&ITaskbarList),
    {
        let mut slot = self.taskbar.borrow_mut();
        if slot.is_none() {
            unsafe {
                if let Ok(tbl) = CoCreateInstance::<_, ITaskbarList>(&TaskbarList, None, CLSCTX_ALL)
                    && tbl.HrInit().is_ok()
                {
                    *slot = Some(tbl);
                }
            }
        }
        if let Some(tbl) = slot.as_ref() {
            f(tbl);
        }
    }
}

/// Brings any transient Windows Shell drag-and-drop feedback windows (`SysDragImage`)
/// to the very head of the `HWND_TOPMOST` z-band so that native drag preview thumbnails
/// remain visible over open floating drawers without being occluded.
pub fn bring_drag_image_to_top() {
    let mut current = HWND::default();
    for _ in 0..8 {
        let found =
            unsafe { FindWindowExW(HWND::default(), current, w!("SysDragImage"), PCWSTR::null()) };
        match found {
            Ok(h) if !h.0.is_null() => {
                let is_already_top = unsafe { GetTopWindow(HWND::default()) == Ok(h) };
                if !is_already_top {
                    unsafe {
                        let _ = SetWindowPos(
                            h,
                            HWND_TOPMOST,
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                        );
                    }
                }
                current = h;
            }
            _ => break,
        }
    }
}

impl WindowSystem for Win32WindowSystem {
    fn get_foreground_window(&self) -> Option<WindowHandle> {
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.0.is_null() {
            None
        } else {
            Some(WindowHandle::from_hwnd(hwnd))
        }
    }

    fn get_cursor_pos(&self) -> Option<(i32, i32)> {
        let mut pt = POINT::default();
        unsafe { GetCursorPos(&mut pt).ok()? };
        Some((pt.x, pt.y))
    }

    fn is_window(&self, hwnd: WindowHandle) -> bool {
        !hwnd.is_null() && unsafe { IsWindow(hwnd.to_hwnd()).as_bool() }
    }

    fn is_window_visible(&self, hwnd: WindowHandle) -> bool {
        !hwnd.is_null() && unsafe { IsWindowVisible(hwnd.to_hwnd()).as_bool() }
    }

    fn snapshot_window(
        &self,
        hwnd: WindowHandle,
    ) -> std::result::Result<WindowSnapshot, WindowAccessError> {
        let raw = hwnd.to_hwnd();
        if hwnd.is_null() || unsafe { !IsWindow(raw).as_bool() } {
            return Err(WindowAccessError::InvalidWindow);
        }

        let mut class_buf = [0u16; 128];
        let class_len = unsafe { GetClassNameW(raw, &mut class_buf) };
        let class_name = String::from_utf16_lossy(&class_buf[..class_len.max(0) as usize]);
        if is_non_fullscreen_shell_or_internal_class(&class_name) {
            return Err(WindowAccessError::ProtectedShellWindow);
        }

        let mut title_buf = [0u16; 256];
        let mut char_count = 0usize;
        let _ = unsafe {
            SendMessageTimeoutW(
                raw,
                WM_GETTEXT,
                WPARAM(title_buf.len()),
                LPARAM(title_buf.as_mut_ptr() as isize),
                SMTO_ABORTIFHUNG,
                50,
                Some(&mut char_count),
            )
        };
        let end = title_buf
            .iter()
            .take(char_count.min(title_buf.len()))
            .position(|&c| c == 0)
            .unwrap_or(char_count.min(title_buf.len()));
        let title = String::from_utf16_lossy(&title_buf[..end]);

        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(raw, Some(&mut pid)) };
        let process_name = query_process_basename_for_pid(pid);

        let mut wr = RECT::default();
        unsafe {
            GetWindowRect(raw, &mut wr).map_err(|_| WindowAccessError::InvalidWindow)?;
        }
        let window_rect: Rect = wr.into();
        let extended_frame_rect = get_extended_frame_bounds(raw).unwrap_or(window_rect);

        let style = unsafe { GetWindowLongPtrW(raw, GWL_STYLE) };
        let ex_style = unsafe { GetWindowLongPtrW(raw, GWL_EXSTYLE) };
        let has_caption = (style & (WS_CAPTION.0 as isize)) == (WS_CAPTION.0 as isize);

        Ok(WindowSnapshot {
            hwnd,
            title,
            process_name,
            class_name,
            pid,
            window_rect,
            extended_frame_rect,
            ex_style,
            has_caption,
        })
    }

    fn get_window_rect(&self, hwnd: WindowHandle) -> Option<Rect> {
        let raw = hwnd.to_hwnd();
        get_extended_frame_bounds(raw).ok().or_else(|| {
            let mut wr = RECT::default();
            unsafe { GetWindowRect(raw, &mut wr).ok().map(|()| wr.into()) }
        })
    }

    fn has_active_child_popup(&self, hwnd: WindowHandle) -> bool {
        has_active_popup_or_menu(hwnd.to_hwnd())
    }

    fn is_window_or_descendant(&self, candidate: WindowHandle, root_target: WindowHandle) -> bool {
        let child = candidate.to_hwnd();
        let target = root_target.to_hwnd();
        if child == target || child.0.is_null() || target.0.is_null() {
            return child == target;
        }
        unsafe {
            if GetAncestor(child, GA_ROOT) == target || GetAncestor(child, GA_ROOTOWNER) == target {
                return true;
            }
            let mut cur = child;
            while !cur.0.is_null() {
                if cur == target {
                    return true;
                }
                match GetParent(cur) {
                    Ok(parent) if !parent.0.is_null() && parent != cur => {
                        cur = parent;
                    }
                    _ => break,
                }
            }
        }
        false
    }

    fn force_restore_foreground_window(&self, target: WindowHandle) {
        force_restore_foreground_window(target.to_hwnd());
    }

    fn set_foreground_window(&self, target: WindowHandle) {
        // Use the AttachThreadInput bridge so hover/IPC reveals defeat Windows Foreground Lock cleanly.
        force_restore_foreground_window(target.to_hwnd());
    }

    fn prepare_window_for_stash(
        &self,
        hwnd: WindowHandle,
        _ex_style: isize,
    ) -> std::result::Result<(), WindowAccessError> {
        let raw = hwnd.to_hwnd();
        unsafe {
            if IsZoomed(raw).as_bool() {
                let _ = ShowWindow(raw, SW_RESTORE);
            }
        }
        // Do NOT apply WS_EX_TOOLWINDOW: Windows DWM strips Minimize and Maximize buttons
        // when WS_EX_TOOLWINDOW is set. Taskbar hiding is handled by ITaskbarList::DeleteTab()!
        self.with_taskbar(|tbl| unsafe {
            let _ = tbl.DeleteTab(raw);
        });
        Ok(())
    }

    fn hide_from_taskbar(&self, hwnd: WindowHandle) {
        let raw = hwnd.to_hwnd();
        self.with_taskbar(|tbl| unsafe {
            let _ = tbl.DeleteTab(raw);
        });
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
        let raw = hwnd.to_hwnd();
        let was_topmost = (original_ex_style & (WS_EX_TOPMOST.0 as isize)) != 0;
        let insert_after = if was_topmost {
            HWND_TOPMOST
        } else {
            HWND_NOTOPMOST
        };
        unsafe {
            let _ = set_window_cloaked(raw, false);
            SetWindowLongPtrW(raw, GWL_EXSTYLE, original_ex_style);
        }
        self.with_taskbar(|tbl| unsafe {
            let _ = tbl.AddTab(raw);
        });

        match target_state {
            UnstashTargetState::Normal => {
                self.ensure_window_restored(hwnd);
                unsafe {
                    let _ = SetWindowPos(
                        raw,
                        insert_after,
                        original_rect.left,
                        original_rect.top,
                        original_rect.width(),
                        original_rect.height(),
                        SWP_NOACTIVATE | SWP_FRAMECHANGED | SWP_NOCOPYBITS,
                    );
                }
                self.refresh_window_rendering(hwnd);
            }
            UnstashTargetState::Minimize => unsafe {
                let mut wp = WINDOWPLACEMENT {
                    length: size_of::<WINDOWPLACEMENT>() as u32,
                    ..Default::default()
                };
                if GetWindowPlacement(raw, &mut wp).is_ok() {
                    wp.rcNormalPosition = RECT {
                        left: original_rect.left,
                        top: original_rect.top,
                        right: original_rect.right,
                        bottom: original_rect.bottom,
                    };
                    let _ = SetWindowPlacement(raw, &wp);
                }
                let _ = ShowWindow(raw, SW_MINIMIZE);
                let _ = PostMessageW(raw, WM_SYSCOMMAND, WPARAM(SC_MINIMIZE as usize), LPARAM(0));
            },
            UnstashTargetState::Maximize => {
                unsafe {
                    let mut wp = WINDOWPLACEMENT {
                        length: size_of::<WINDOWPLACEMENT>() as u32,
                        ..Default::default()
                    };
                    if GetWindowPlacement(raw, &mut wp).is_ok() {
                        wp.rcNormalPosition = RECT {
                            left: original_rect.left,
                            top: original_rect.top,
                            right: original_rect.right,
                            bottom: original_rect.bottom,
                        };
                        let _ = SetWindowPlacement(raw, &wp);
                    }
                    let _ = ShowWindow(raw, SW_MAXIMIZE);
                    let _ =
                        PostMessageW(raw, WM_SYSCOMMAND, WPARAM(SC_MAXIMIZE as usize), LPARAM(0));
                }
                self.refresh_window_rendering(hwnd);
            }
        }
    }

    fn set_window_topmost(&self, hwnd: WindowHandle, topmost: bool) {
        let raw = hwnd.to_hwnd();
        let insert_after = if topmost {
            HWND_TOPMOST
        } else {
            HWND_NOTOPMOST
        };
        unsafe {
            let _ = SetWindowPos(
                raw,
                insert_after,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
        if topmost {
            bring_drag_image_to_top();
        }
    }

    fn close_window(&self, hwnd: WindowHandle) {
        unsafe {
            let _ = PostMessageW(hwnd.to_hwnd(), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }

    fn ensure_window_restored(&self, hwnd: WindowHandle) {
        let raw = hwnd.to_hwnd();
        unsafe {
            if IsIconic(raw).as_bool() {
                let _ = ShowWindow(raw, SW_RESTORE);
            }
        }
    }

    fn refresh_window_rendering(&self, hwnd: WindowHandle) {
        let raw = hwnd.to_hwnd();
        unsafe {
            let _ = RedrawWindow(
                raw,
                None,
                None,
                RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_UPDATENOW | RDW_FRAME | RDW_ERASE,
            );

            // If it's Windows Explorer, trigger a 1-pixel micro-nudge with SWP_NOCOPYBITS | SWP_FRAMECHANGED
            // to force Explorer's internal DirectComposition swapchains in the navigation pane to reallocate
            // and eliminate black unpainted rectangular regions.
            let mut class_buf = [0u16; 64];
            let len = GetClassNameW(raw, &mut class_buf);
            if len > 0 {
                let class_name = String::from_utf16_lossy(&class_buf[..len as usize]);
                if class_name == "CabinetWClass" || class_name == "ExploreWClass" {
                    let mut r = RECT::default();
                    if GetWindowRect(raw, &mut r).is_ok() {
                        let w = r.right - r.left;
                        let h = r.bottom - r.top;
                        if w > 2 && h > 2 {
                            let _ = SetWindowPos(
                                raw,
                                None,
                                r.left,
                                r.top,
                                w + 1,
                                h,
                                SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOCOPYBITS | SWP_FRAMECHANGED,
                            );
                            let _ = SetWindowPos(
                                raw,
                                None,
                                r.left,
                                r.top,
                                w,
                                h,
                                SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOCOPYBITS | SWP_FRAMECHANGED,
                            );
                        }
                    }
                }
            }
        }
    }

    fn set_window_rect_visible(&self, hwnd: WindowHandle, visible_rect: Rect) {
        let raw = hwnd.to_hwnd();
        self.ensure_window_restored(hwnd);
        let insets = get_invisible_border_insets(raw).unwrap_or_default();
        let wr = visible_to_window_rect(visible_rect, insets);
        unsafe {
            let mut wp = WINDOWPLACEMENT {
                length: size_of::<WINDOWPLACEMENT>() as u32,
                ..Default::default()
            };
            if GetWindowPlacement(raw, &mut wp).is_ok() {
                wp.rcNormalPosition = RECT {
                    left: wr.left,
                    top: wr.top,
                    right: wr.right,
                    bottom: wr.bottom,
                };
                let _ = SetWindowPlacement(raw, &wp);
            }

            let _ = SetWindowPos(
                raw,
                HWND_TOPMOST,
                wr.left,
                wr.top,
                wr.width(),
                wr.height(),
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOCOPYBITS | SWP_FRAMECHANGED,
            );
        }
        self.refresh_window_rendering(hwnd);
    }

    fn park_and_cloak_window(&self, hwnd: WindowHandle, parked_origin: (i32, i32)) {
        let raw = hwnd.to_hwnd();
        let insets = get_invisible_border_insets(raw).unwrap_or_default();
        let (wx, wy) = visible_to_window_origin(parked_origin.0, parked_origin.1, insets);
        unsafe {
            let _ = set_window_cloaked(raw, true);
            let _ = SetWindowPos(
                raw,
                HWND_TOPMOST,
                wx,
                wy,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOCOPYBITS,
            );
        }
    }

    fn uncloak_window(&self, hwnd: WindowHandle) {
        let _ = set_window_cloaked(hwnd.to_hwnd(), false);
    }

    fn is_window_cloaked(&self, hwnd: WindowHandle) -> bool {
        is_window_cloaked(hwnd.to_hwnd())
    }

    fn is_window_zoomed(&self, hwnd: WindowHandle) -> bool {
        unsafe { IsZoomed(hwnd.to_hwnd()).as_bool() }
    }

    fn animate_transitions(
        &self,
        transitions: &[WindowTransition],
        duration_ms: u32,
        style: AnimationStyle,
        is_opening: bool,
    ) {
        let targets: Vec<Win32TransitionTarget> = transitions
            .iter()
            .map(|t| Win32TransitionTarget {
                hwnd: t.hwnd.to_hwnd(),
                start_visible_x: t.start_visible_x,
                start_visible_y: t.start_visible_y,
                end_visible_x: t.end_visible_x,
                end_visible_y: t.end_visible_y,
            })
            .collect();
        animate_windows_transition(&targets, duration_ms, style, is_opening);
    }

    fn enumerate_monitors(&self) -> Vec<MonitorInfo> {
        unsafe extern "system" fn enum_mon_proc(
            hmon: HMONITOR,
            _hdc: HDC,
            _lprc: *mut RECT,
            lparam: LPARAM,
        ) -> BOOL {
            let out = unsafe { &mut *(lparam.0 as *mut Vec<MonitorInfo>) };
            let mut info = MONITORINFOEXW::default();
            info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
            if unsafe { GetMonitorInfoW(hmon, &mut info.monitorInfo as *mut _).as_bool() } {
                let name_end = info
                    .szDevice
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(info.szDevice.len());
                let id = String::from_utf16_lossy(&info.szDevice[..name_end]);
                let mut dpi_x = 96u32;
                let mut dpi_y = 96u32;
                let _ =
                    unsafe { GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
                out.push(MonitorInfo {
                    id,
                    rect: info.monitorInfo.rcMonitor.into(),
                    work_rect: info.monitorInfo.rcWork.into(),
                    dpi: dpi_x.max(96),
                    is_primary: (info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY) != 0,
                });
            }
            BOOL(1)
        }

        let mut monitors: Vec<MonitorInfo> = Vec::new();
        unsafe {
            let _ = EnumDisplayMonitors(
                None,
                None,
                Some(enum_mon_proc),
                LPARAM(&mut monitors as *mut _ as isize),
            );
        }
        monitors.sort_by_key(|m| !m.is_primary);
        monitors
    }

    fn reset_taskbar(&self) {
        *self.taskbar.borrow_mut() = None;
    }

    fn is_fullscreen_on_rect(&self, target_rect: Option<Rect>) -> bool {
        unsafe {
            let fg = GetForegroundWindow();
            if fg.0.is_null() || fg == GetShellWindow() || fg == GetDesktopWindow() {
                return false;
            }
            let mut class_buf = [0u16; 128];
            let class_len = GetClassNameW(fg, &mut class_buf);
            if class_len > 0 {
                let class_name = String::from_utf16_lossy(&class_buf[..class_len as usize]);
                if is_non_fullscreen_shell_or_internal_class(&class_name) {
                    return false;
                }
            }
            let style = GetWindowLongPtrW(fg, GWL_STYLE);
            let has_caption = (style & (WS_CAPTION.0 as isize)) == (WS_CAPTION.0 as isize);
            if has_caption {
                return false;
            }

            let mut wr = RECT::default();
            if GetWindowRect(fg, &mut wr).is_err() {
                return false;
            }
            let hmon = MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(hmon, &mut mi).as_bool() {
                let mon_r: Rect = mi.rcMonitor.into();
                let win_r: Rect = wr.into();
                let is_full = win_r.left <= mon_r.left
                    && win_r.top <= mon_r.top
                    && win_r.right >= mon_r.right
                    && win_r.bottom >= mon_r.bottom;
                if !is_full {
                    return false;
                }
                if let Some(target) = target_rect {
                    return !(mon_r.right <= target.left
                        || mon_r.left >= target.right
                        || mon_r.bottom <= target.top
                        || mon_r.top >= target.bottom);
                }
                return true;
            }
            false
        }
    }

    fn is_fullscreen_active(&self) -> bool {
        self.is_fullscreen_on_rect(None)
    }
}

/// Real COM [`VirtualDesktopApi`] implementation over [`VirtualDesktopController`].
pub struct Win32VirtualDesktop {
    inner: VirtualDesktopController,
}

impl Win32VirtualDesktop {
    pub fn new(helper_hwnd: HWND) -> Result<Self> {
        Ok(Self {
            inner: VirtualDesktopController::new(helper_hwnd)?,
        })
    }
}

impl VirtualDesktopApi for Win32VirtualDesktop {
    fn get_current_desktop_id(&self) -> Option<GUID> {
        self.inner.get_current_desktop_id()
    }

    fn get_window_desktop_id(&self, hwnd: WindowHandle) -> Option<GUID> {
        self.inner.get_window_desktop_id(hwnd.to_hwnd())
    }

    fn is_on_current_desktop(&self, hwnd: WindowHandle) -> bool {
        self.inner.is_on_current_desktop(hwnd.to_hwnd())
    }

    fn move_window_to_desktop(&self, hwnd: WindowHandle, desktop_id: &GUID) -> bool {
        self.inner
            .move_window_to_desktop(hwnd.to_hwnd(), desktop_id)
            .is_ok()
    }
}

unsafe extern "system" fn appbar_helper_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == crate::appbar::WM_APP_APPBAR {
        let parent = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
        if parent != 0 {
            unsafe {
                let _ = PostMessageW(HWND(parent as *mut _), msg, wparam, lparam);
            }
        }
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn ensure_appbar_class_registered(inst: windows::Win32::Foundation::HINSTANCE) {
    static REGISTERED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    REGISTERED.get_or_init(|| {
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(appbar_helper_wndproc),
            hInstance: inst,
            lpszClassName: w!("EdgeStashAppBarHelperClass"),
            ..Default::default()
        };
        unsafe {
            let _ = RegisterClassExW(&wc);
        }
    });
}

/// Real Win32 [`AppBarApi`] implementation assigning one AppBar helper HWND per `(monitor_id, edge)`.
pub struct Win32AppBar {
    helpers: RefCell<HashMap<(String, ScreenEdge), HWND>>,
    fallback_hwnd: HWND,
}

impl Win32AppBar {
    pub fn new(fallback_hwnd: HWND) -> Self {
        Self {
            helpers: RefCell::new(HashMap::new()),
            fallback_hwnd,
        }
    }

    pub fn register_helper(&self, monitor_id: String, edge: ScreenEdge, hwnd: HWND) {
        self.helpers.borrow_mut().insert((monitor_id, edge), hwnd);
    }

    fn ensure_helper_hwnd(&self, monitor_id: &str, edge: ScreenEdge) -> HWND {
        let key = (monitor_id.to_string(), edge);
        if let Some(&existing) = self.helpers.borrow().get(&key)
            && !existing.0.is_null()
        {
            return existing;
        }
        let hinst: Option<windows::Win32::Foundation::HINSTANCE> =
            unsafe { GetModuleHandleW(None).ok().map(|h| h.into()) };
        if let Some(inst) = hinst {
            ensure_appbar_class_registered(inst);
            if let Ok(hwnd) = unsafe {
                CreateWindowExW(
                    WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    w!("EdgeStashAppBarHelperClass"),
                    w!("EdgeStashAppBarHelper"),
                    WS_POPUP,
                    -32000,
                    -32000,
                    1,
                    1,
                    None,
                    None,
                    inst,
                    None,
                )
            } {
                unsafe {
                    let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, self.fallback_hwnd.0 as isize);
                }
                self.helpers.borrow_mut().insert(key, hwnd);
                return hwnd;
            }
        }
        self.fallback_hwnd
    }
}

impl Drop for Win32AppBar {
    fn drop(&mut self) {
        let helpers: Vec<HWND> = self.helpers.get_mut().drain().map(|(_, h)| h).collect();
        for hwnd in helpers {
            if !hwnd.0.is_null() {
                let _ = register_or_update_native_dock(
                    hwnd,
                    ScreenEdge::Right,
                    Rect::default(),
                    1,
                    false,
                );
                if hwnd != self.fallback_hwnd {
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                }
            }
        }
    }
}

impl AppBarApi for Win32AppBar {
    fn register_or_update_dock(
        &self,
        monitor_id: &str,
        edge: ScreenEdge,
        monitor_rect: Rect,
        thickness_px: i32,
        enable: bool,
    ) -> Option<Rect> {
        let key = (monitor_id.to_string(), edge);
        if !enable {
            let removed = self.helpers.borrow_mut().remove(&key);
            let hwnd = removed.unwrap_or(self.fallback_hwnd);
            if !hwnd.0.is_null() {
                let _ =
                    register_or_update_native_dock(hwnd, edge, monitor_rect, thickness_px, false);
                if removed.is_some() && hwnd != self.fallback_hwnd {
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                }
            }
            return None;
        }
        let hwnd = self.ensure_helper_hwnd(monitor_id, edge);
        if hwnd.0.is_null() {
            return None;
        }
        register_or_update_native_dock(hwnd, edge, monitor_rect, thickness_px, true)
    }
}
