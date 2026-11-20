pub mod win32;

use std::cell::Cell;

use edgestash_ipc::types::{DockMode, Rect, ScreenEdge};
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::HWND;
use windows::core::GUID;

/// Portable window handle identifier wrapping Win32 `HWND` (`isize`) for deterministic testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct WindowHandle(pub isize);

impl WindowHandle {
    pub const NULL: Self = Self(0);

    pub fn is_null(self) -> bool {
        self.0 == 0
    }

    pub fn to_hwnd(self) -> HWND {
        HWND(self.0 as *mut _)
    }

    pub fn from_hwnd(hwnd: HWND) -> Self {
        Self(hwnd.0 as isize)
    }
}

/// Stable monitor descriptor including physical bounds, work area (excluding taskbar/AppBars), and DPI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorInfo {
    pub id: String,
    pub rect: Rect,
    pub work_rect: Rect,
    pub dpi: u32,
    pub is_primary: bool,
}

/// Monotonic millisecond clock abstraction enabling deterministic timer & gesture tests.
pub trait Clock {
    fn now_ms(&self) -> u64;
}

/// Real monotonic clock backed by `std::time::Instant`.
pub struct SystemClock {
    origin: std::time::Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            origin: std::time::Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        self.origin.elapsed().as_millis() as u64
    }
}

/// Deterministic manual clock for unit tests.
#[derive(Debug, Default)]
pub struct FakeClock {
    now: Cell<u64>,
}

impl FakeClock {
    pub fn new(initial_ms: u64) -> Self {
        Self {
            now: Cell::new(initial_ms),
        }
    }

    pub fn advance(&self, delta_ms: u64) {
        self.now.set(self.now.get() + delta_ms);
    }

    pub fn set(&self, ms: u64) {
        self.now.set(ms);
    }
}

impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.now.get()
    }
}

/// Abstract window capture metadata queried when stashing a window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowSnapshot {
    pub hwnd: WindowHandle,
    pub title: String,
    pub process_name: String,
    pub class_name: String,
    pub pid: u32,
    pub window_rect: Rect,
    pub extended_frame_rect: Rect,
    pub ex_style: isize,
    pub has_caption: bool,
}

/// Outcome of querying window mutability/eligibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowAccessError {
    InvalidWindow,
    ProtectedShellWindow,
    AccessDenied,
}

/// Transition specification for animating one window in a drawer or group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowTransition {
    pub hwnd: WindowHandle,
    pub start_visible_x: i32,
    pub start_visible_y: i32,
    pub end_visible_x: i32,
    pub end_visible_y: i32,
}

/// Target state for a window after un-stashing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnstashTargetState {
    /// Window is restored to its original desktop rectangle.
    Normal,
    /// Window is minimized into the taskbar.
    Minimize,
    /// Window is maximized across the desktop work area.
    Maximize,
}

/// Platform port isolating Win32 / DWM / Taskbar / Display queries from engine state logic.
pub trait WindowSystem {
    fn get_foreground_window(&self) -> Option<WindowHandle>;
    fn get_cursor_pos(&self) -> Option<(i32, i32)>;
    fn is_window(&self, hwnd: WindowHandle) -> bool;
    fn is_window_visible(&self, hwnd: WindowHandle) -> bool;
    fn snapshot_window(&self, hwnd: WindowHandle) -> Result<WindowSnapshot, WindowAccessError>;
    fn get_window_rect(&self, hwnd: WindowHandle) -> Option<Rect>;
    fn has_active_child_popup(&self, hwnd: WindowHandle) -> bool;
    fn is_window_or_descendant(&self, candidate: WindowHandle, root_target: WindowHandle) -> bool {
        candidate == root_target
    }
    fn force_restore_foreground_window(&self, target: WindowHandle);
    fn set_foreground_window(&self, target: WindowHandle);

    fn prepare_window_for_stash(
        &self,
        hwnd: WindowHandle,
        ex_style: isize,
    ) -> Result<(), WindowAccessError>;
    fn hide_from_taskbar(&self, _hwnd: WindowHandle) {}
    fn restore_window_from_stash(
        &self,
        hwnd: WindowHandle,
        original_ex_style: isize,
        original_rect: Rect,
    );
    fn restore_window_from_stash_with_state(
        &self,
        hwnd: WindowHandle,
        original_ex_style: isize,
        original_rect: Rect,
        _target_state: UnstashTargetState,
    ) {
        self.restore_window_from_stash(hwnd, original_ex_style, original_rect);
    }
    fn set_window_rect_visible(&self, hwnd: WindowHandle, visible_rect: Rect);
    fn set_window_topmost(&self, _hwnd: WindowHandle, _topmost: bool) {}
    fn close_window(&self, _hwnd: WindowHandle) {}
    fn ensure_window_restored(&self, _hwnd: WindowHandle) {}
    fn refresh_window_rendering(&self, _hwnd: WindowHandle) {}
    fn is_window_zoomed(&self, _hwnd: WindowHandle) -> bool {
        false
    }
    fn park_and_cloak_window(&self, hwnd: WindowHandle, parked_origin: (i32, i32));
    fn uncloak_window(&self, hwnd: WindowHandle);
    fn is_window_cloaked(&self, hwnd: WindowHandle) -> bool;

    fn animate_transitions(
        &self,
        transitions: &[WindowTransition],
        duration_ms: u32,
        style: edgestash_ipc::types::AnimationStyle,
        is_opening: bool,
    );

    fn enumerate_monitors(&self) -> Vec<MonitorInfo>;
    fn is_fullscreen_active(&self) -> bool;
}

/// Platform port for Virtual Desktop operations (`IVirtualDesktopManager`).
pub trait VirtualDesktopApi {
    fn get_current_desktop_id(&self) -> Option<GUID>;
    fn get_window_desktop_id(&self, hwnd: WindowHandle) -> Option<GUID>;
    fn is_on_current_desktop(&self, hwnd: WindowHandle) -> bool;
    fn move_window_to_desktop(&self, hwnd: WindowHandle, desktop_id: &GUID) -> bool;
}

/// Platform port for `SHAppBarMessage` native work-area docking.
pub trait AppBarApi {
    fn register_or_update_dock(
        &self,
        monitor_id: &str,
        edge: ScreenEdge,
        monitor_rect: Rect,
        thickness_px: i32,
        enable: bool,
    ) -> Option<Rect>;
}

/// Event emitted to the edge pill / overlay renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PillOverlayEvent {
    SyncState {
        monitor_id: String,
        edge: ScreenEdge,
        slot_count: usize,
        active_index: usize,
        window_handles: Vec<WindowHandle>,
        slot_titles: Vec<String>,
        is_open: bool,
        needs_attention: Vec<bool>,
        attention_count: u32,
    },
    ShowDrawerHeader {
        monitor_id: String,
        edge: ScreenEdge,
        header_rect: Rect,
        title: String,
        slot_index: usize,
        slot_count: usize,
        dock_mode: DockMode,
        preferred_pin_mode: DockMode,
        is_grouped: bool,
    },
    HideDrawerHeader {
        monitor_id: String,
        edge: ScreenEdge,
    },
    WindowClosedAnimation {
        monitor_id: String,
        edge: ScreenEdge,
        closed_hwnd: WindowHandle,
        closed_title: String,
    },
    ShowDragPreview {
        monitor_id: String,
        edge: ScreenEdge,
        band_rect: Rect,
        active_target: Option<crate::trigger::DragDropTarget>,
        edge_zone_px: i32,
        corner_span_px: i32,
    },
    HideDragPreview,
    SetDimScrim {
        visible: bool,
        monitor_rect: Rect,
        opacity_percent: u8,
    },
}

/// Platform port for Direct2D/DirectComposition overlay surfaces.
pub trait VisualOverlayApi {
    fn emit_overlay_event(&self, event: PillOverlayEvent);

    fn header_hwnd_for_edge(&self, _monitor_id: &str, _edge: ScreenEdge) -> Option<WindowHandle> {
        None
    }
}
