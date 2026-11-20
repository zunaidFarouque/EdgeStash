use edgestash_ipc::config::EdgeStashConfig;
use edgestash_ipc::types::{DockMode, Rect};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, SetFocus, VK_MENU, keybd_event,
};
use windows::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, BringWindowToTop, EnumWindows, GetClassNameW, GetCursorPos,
    GetForegroundWindow, GetLastActivePopup, GetWindowRect, GetWindowThreadProcessId, IsWindow,
    IsWindowVisible, SW_SHOW, SetForegroundWindow, ShowWindow, SwitchToThisWindow,
};

use crate::platform::WindowHandle;

/// Checks if the stashed window currently has an open context menu, dropdown, or modal dialog.
/// If true, EdgeStash MUST NOT auto-hide the drawer even if the cursor leaves the main rect.
pub fn has_active_child_popup(stashed_hwnd: HWND) -> bool {
    unsafe {
        let popup = GetLastActivePopup(stashed_hwnd);
        if popup != stashed_hwnd && IsWindowVisible(popup).as_bool() {
            return true;
        }
        false
    }
}

/// Extends `has_active_child_popup` by also detecting Win32 `#32768` popup/context menus
/// owned by the same process as `stashed_hwnd`.
pub fn has_active_popup_or_menu(stashed_hwnd: HWND) -> bool {
    if has_active_child_popup(stashed_hwnd) {
        return true;
    }

    unsafe {
        let mut target_pid = 0u32;
        GetWindowThreadProcessId(stashed_hwnd, Some(&mut target_pid));
        if target_pid == 0 {
            return false;
        }

        struct SearchState {
            target_pid: u32,
            found: bool,
        }

        unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let state = unsafe { &mut *(lparam.0 as *mut SearchState) };
            if unsafe { !IsWindowVisible(hwnd).as_bool() } {
                return BOOL(1);
            }
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
            if pid != state.target_pid {
                return BOOL(1);
            }

            let mut class_buf = [0u16; 32];
            let len = unsafe { GetClassNameW(hwnd, &mut class_buf) };
            if len > 0 {
                let class_name = String::from_utf16_lossy(&class_buf[..len as usize]);
                if class_name == "#32768" {
                    state.found = true;
                    return BOOL(0);
                }
            }
            BOOL(1)
        }

        let mut state = SearchState {
            target_pid,
            found: false,
        };
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut state as *mut _ as isize));
        state.found
    }
}

/// Checks if the cursor is inside the stashed window + a hysteresis margin (e.g. 24px).
pub fn is_cursor_inside_with_margin(hwnd: HWND, margin_px: i32) -> bool {
    unsafe {
        let mut pt = POINT::default();
        if GetCursorPos(&mut pt).is_err() {
            return false;
        }
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return false;
        }
        pt.x >= (rect.left - margin_px)
            && pt.x <= (rect.right + margin_px)
            && pt.y >= (rect.top - margin_px)
            && pt.y <= (rect.bottom + margin_px)
    }
}

/// Reliably restores keyboard focus to `target_hwnd` without triggering Windows Foreground Lock.
pub fn force_restore_foreground_window(target_hwnd: HWND) {
    unsafe {
        if !IsWindow(target_hwnd).as_bool() || !IsWindowVisible(target_hwnd).as_bool() {
            return;
        }

        let current_fg = GetForegroundWindow();
        if current_fg == target_hwnd {
            let _ = SetFocus(target_hwnd);
            return;
        }

        let fg_thread = GetWindowThreadProcessId(current_fg, None);
        let target_thread = GetWindowThreadProcessId(target_hwnd, None);
        let current_thread = GetCurrentThreadId();

        // 1. Attach input queues across calling thread, current foreground thread, and target thread
        if fg_thread != 0 && target_thread != 0 && fg_thread != target_thread {
            let _ = AttachThreadInput(fg_thread, target_thread, true);
        }
        if fg_thread != 0 && fg_thread != current_thread {
            let _ = AttachThreadInput(current_thread, fg_thread, true);
        }
        if target_thread != 0 && target_thread != current_thread {
            let _ = AttachThreadInput(current_thread, target_thread, true);
        }

        // 2. Grant unconditional foreground permission and pulse Alt key (VK_MENU) to defeat Windows Foreground Lock
        let _ = AllowSetForegroundWindow(ASFW_ANY);
        keybd_event(VK_MENU.0 as u8, 0, KEYBD_EVENT_FLAGS(0), 0);
        keybd_event(VK_MENU.0 as u8, 0, KEYEVENTF_KEYUP, 0);

        let _ = BringWindowToTop(target_hwnd);
        let _ = ShowWindow(target_hwnd, SW_SHOW);
        let success = SetForegroundWindow(target_hwnd).as_bool();
        if !success {
            SwitchToThisWindow(target_hwnd, true);
        }
        let _ = SetFocus(target_hwnd);

        // 3. Detach thread input queues cleanly
        if fg_thread != 0 && target_thread != 0 && fg_thread != target_thread {
            let _ = AttachThreadInput(fg_thread, target_thread, false);
        }
        if fg_thread != 0 && fg_thread != current_thread {
            let _ = AttachThreadInput(current_thread, fg_thread, false);
        }
        if target_thread != 0 && target_thread != current_thread {
            let _ = AttachThreadInput(current_thread, target_thread, false);
        }
    }
}

/// Hysteresis margin in pixels around an open drawer before mouse-leave countdown begins.
pub const DEFAULT_HYSTERESIS_MARGIN_PX: i32 = 24;

/// Action emitted by [`DismissalController`] when monitoring an open drawer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DismissalAction {
    None,
    HideImmediate,
    HideAndRestoreFocus {
        restore_target: Option<WindowHandle>,
        delay_ms: u32,
    },
}

/// Pure state machine governing mouse-leave hysteresis, popup guard, focus-loss,
/// and deterministic focus restoration for the active drawer (single or grouped).
#[derive(Debug, Default, Clone)]
pub struct DismissalController {
    previous_foreground: Option<WindowHandle>,
    dock_mode: Option<DockMode>,
    has_cursor_entered_since_open: bool,
    leave_countdown_started_at: Option<u64>,
    drag_in_progress: bool,
    has_gained_focus: bool,
    opened_at_ms: u64,
}

impl DismissalController {
    /// Resets state when a drawer or grouped slot opens.
    pub fn on_drawer_opened(
        &mut self,
        previous_foreground: Option<WindowHandle>,
        dock_mode: DockMode,
    ) {
        self.previous_foreground = previous_foreground;
        self.dock_mode = Some(dock_mode);
        self.has_cursor_entered_since_open = false;
        self.leave_countdown_started_at = None;
        self.has_gained_focus = false;
        self.opened_at_ms = 0;
    }

    /// Resets state when a drawer opens with timestamp for grace period protection.
    pub fn on_drawer_opened_at(
        &mut self,
        previous_foreground: Option<WindowHandle>,
        dock_mode: DockMode,
        opened_at_ms: u64,
    ) {
        self.on_drawer_opened(previous_foreground, dock_mode);
        self.opened_at_ms = opened_at_ms;
    }

    /// Sets the timestamp when drawer was opened to govern the initial focus grace period.
    pub fn set_opened_at_ms(&mut self, opened_at_ms: u64) {
        self.opened_at_ms = opened_at_ms;
    }

    /// Marks that the stashed drawer window (or one of its controls) has actively gained foreground focus.
    pub fn mark_drawer_gained_focus(&mut self) {
        self.has_gained_focus = true;
    }

    /// Marks pointer/drag as having already engaged the drawer/pill so subsequent leaving can dismiss after drag ends.
    pub fn mark_pointer_entered(&mut self) {
        self.has_cursor_entered_since_open = true;
    }

    /// Suspends automatic mouse-leave/focus-loss dismissal while an OLE drag-to-pill operation is in flight.
    pub fn set_drag_in_progress(&mut self, active: bool) {
        self.drag_in_progress = active;
        if active {
            self.has_cursor_entered_since_open = true;
            self.leave_countdown_started_at = None;
        }
    }

    /// Clears state when the drawer closes.
    pub fn reset(&mut self) {
        self.previous_foreground = None;
        self.dock_mode = None;
        self.has_cursor_entered_since_open = false;
        self.leave_countdown_started_at = None;
        self.has_gained_focus = false;
        self.opened_at_ms = 0;
    }

    /// Evaluates cursor position (`30Hz` check while open) and popup guard state.
    pub fn tick(
        &mut self,
        cursor: (i32, i32),
        group_window_rects: &[Rect],
        has_active_popup: bool,
        now_ms: u64,
        cfg: &EdgeStashConfig,
    ) -> DismissalAction {
        if self.dock_mode != Some(DockMode::FloatingOverlay) {
            return DismissalAction::None;
        }

        if self.drag_in_progress {
            self.has_cursor_entered_since_open = true;
            self.leave_countdown_started_at = None;
            return DismissalAction::None;
        }

        let inside_exact = group_window_rects
            .iter()
            .any(|r| r.contains_inclusive(cursor.0, cursor.1));
        if inside_exact {
            self.has_cursor_entered_since_open = true;
            self.leave_countdown_started_at = None;
            return DismissalAction::None;
        }

        if !self.has_cursor_entered_since_open || !cfg.hide_on_mouse_leave {
            return DismissalAction::None;
        }

        if has_active_popup {
            self.leave_countdown_started_at = None;
            return DismissalAction::None;
        }

        let inside_with_margin = group_window_rects.iter().any(|r| {
            r.expanded(DEFAULT_HYSTERESIS_MARGIN_PX)
                .contains_inclusive(cursor.0, cursor.1)
        });

        if inside_with_margin {
            self.leave_countdown_started_at = None;
            return DismissalAction::None;
        }

        match self.leave_countdown_started_at {
            None => {
                self.leave_countdown_started_at = Some(now_ms);
                DismissalAction::None
            }
            Some(started_at) => {
                if now_ms.saturating_sub(started_at) >= u64::from(cfg.mouse_leave_delay_ms) {
                    let restore_target = if cfg.restore_prev_focus {
                        self.previous_foreground
                    } else {
                        None
                    };
                    self.reset();
                    DismissalAction::HideAndRestoreFocus {
                        restore_target,
                        delay_ms: cfg.focus_restore_delay_ms,
                    }
                } else {
                    DismissalAction::None
                }
            }
        }
    }

    /// Evaluates `EVENT_SYSTEM_FOREGROUND` focus changes with cursor location and grace period protection.
    pub fn on_foreground_changed_at(
        &mut self,
        new_foreground: WindowHandle,
        open_group_windows: &[WindowHandle],
        overlay_windows: &[WindowHandle],
        cursor_over_drawer: bool,
        now_ms: u64,
        cfg: &EdgeStashConfig,
    ) -> DismissalAction {
        if self.dock_mode != Some(DockMode::FloatingOverlay)
            || !cfg.hide_on_focus_loss
            || self.drag_in_progress
        {
            return DismissalAction::None;
        }
        if open_group_windows.contains(&new_foreground) || overlay_windows.contains(&new_foreground)
        {
            self.has_gained_focus = true;
            return DismissalAction::None;
        }
        if cursor_over_drawer {
            return DismissalAction::None;
        }
        if !self.has_gained_focus && now_ms > 0 && now_ms.saturating_sub(self.opened_at_ms) < 500 {
            return DismissalAction::None;
        }
        self.reset();
        DismissalAction::HideImmediate
    }

    /// Evaluates `EVENT_SYSTEM_FOREGROUND` focus changes while a drawer is open.
    pub fn on_foreground_changed(
        &mut self,
        new_foreground: WindowHandle,
        open_group_windows: &[WindowHandle],
        overlay_windows: &[WindowHandle],
        cfg: &EdgeStashConfig,
    ) -> DismissalAction {
        self.on_foreground_changed_at(
            new_foreground,
            open_group_windows,
            overlay_windows,
            false,
            0,
            cfg,
        )
    }
}
