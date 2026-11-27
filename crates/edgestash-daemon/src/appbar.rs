use std::mem::size_of;

use edgestash_ipc::types::{Rect, ScreenEdge};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::{
    ABE_BOTTOM, ABE_LEFT, ABE_RIGHT, ABE_TOP, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS,
    APPBARDATA, SHAppBarMessage,
};

pub const WM_APP_APPBAR: u32 = 0x8000 + 17; // WM_APP + 17

/// Registers, updates, or removes an OS Application Desktop Toolbar (`SHAppBarMessage`)
/// so Windows shrinks the desktop work area and pushes maximized/snapped windows aside.
/// Returns the final desktop rectangle reserved for this AppBar when `enable` is `true`.
pub fn register_or_update_native_dock(
    helper_hwnd: HWND,
    edge: ScreenEdge,
    monitor_rect: Rect,
    dock_thickness_px: i32,
    enable: bool,
) -> Option<Rect> {
    unsafe {
        let mut abd = APPBARDATA {
            cbSize: size_of::<APPBARDATA>() as u32,
            hWnd: helper_hwnd,
            uCallbackMessage: WM_APP_APPBAR,
            ..Default::default()
        };

        if !enable {
            SHAppBarMessage(ABM_REMOVE, &mut abd);
            return None;
        }

        SHAppBarMessage(ABM_NEW, &mut abd);

        abd.uEdge = match edge {
            ScreenEdge::Left => ABE_LEFT,
            ScreenEdge::Right => ABE_RIGHT,
            ScreenEdge::Top => ABE_TOP,
            ScreenEdge::Bottom => ABE_BOTTOM,
        };

        let clamp_thick = dock_thickness_px.max(1);
        abd.rc = monitor_rect.into();
        match edge {
            ScreenEdge::Left => abd.rc.right = abd.rc.left + clamp_thick,
            ScreenEdge::Right => abd.rc.left = abd.rc.right - clamp_thick,
            ScreenEdge::Top => abd.rc.bottom = abd.rc.top + clamp_thick,
            ScreenEdge::Bottom => abd.rc.top = abd.rc.bottom - clamp_thick,
        }

        // Query first so Explorer adjusts perpendicular bounds for the taskbar, then re-lock our thickness
        SHAppBarMessage(ABM_QUERYPOS, &mut abd);
        match edge {
            ScreenEdge::Left => abd.rc.right = abd.rc.left + clamp_thick,
            ScreenEdge::Right => abd.rc.left = abd.rc.right - clamp_thick,
            ScreenEdge::Top => abd.rc.bottom = abd.rc.top + clamp_thick,
            ScreenEdge::Bottom => abd.rc.top = abd.rc.bottom - clamp_thick,
        }
        SHAppBarMessage(ABM_SETPOS, &mut abd);
        Some(Rect::from(abd.rc))
    }
}
