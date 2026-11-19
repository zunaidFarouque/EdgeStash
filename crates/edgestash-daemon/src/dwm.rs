use std::mem::size_of;

use edgestash_ipc::types::Rect;
use windows::Win32::Foundation::{BOOL, HWND, RECT};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAK, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
    DwmSetWindowAttribute,
};
use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
use windows::core::Result;

/// Thickness of Windows 10/11 invisible DWM shadow borders in physical pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BorderInsets {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl BorderInsets {
    /// Computes invisible shadow insets from `GetWindowRect` and `DWMWA_EXTENDED_FRAME_BOUNDS`.
    pub fn from_rects(window_rect: Rect, extended_frame_rect: Rect) -> Self {
        Self {
            left: extended_frame_rect.left - window_rect.left,
            top: extended_frame_rect.top - window_rect.top,
            right: window_rect.right - extended_frame_rect.right,
            bottom: window_rect.bottom - extended_frame_rect.bottom,
        }
    }
}

/// Calculates the exact invisible DWM shadow border thickness in physical pixels.
pub fn get_invisible_border_insets(hwnd: HWND) -> Result<BorderInsets> {
    unsafe {
        let mut window_rect = RECT::default();
        GetWindowRect(hwnd, &mut window_rect)?;

        let mut frame_rect = RECT::default();
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut frame_rect as *mut _ as *mut _,
            size_of::<RECT>() as u32,
        )?;

        Ok(BorderInsets::from_rects(window_rect.into(), frame_rect.into()))
    }
}

/// Queries `DWMWA_EXTENDED_FRAME_BOUNDS` directly.
pub fn get_extended_frame_bounds(hwnd: HWND) -> Result<Rect> {
    unsafe {
        let mut frame_rect = RECT::default();
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut frame_rect as *mut _ as *mut _,
            size_of::<RECT>() as u32,
        )?;
        Ok(frame_rect.into())
    }
}

/// Cloaks or uncloaks a window via DWM.
/// A cloaked window remains warm in GPU memory (no redraw flash on uncloak),
/// consumes 0 compositor bandwidth, and NEVER bleeds onto adjacent monitors!
pub fn set_window_cloaked(hwnd: HWND, cloaked: bool) -> Result<()> {
    unsafe {
        let value: BOOL = BOOL::from(cloaked);
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_CLOAK,
            &value as *const _ as *const _,
            size_of::<BOOL>() as u32,
        )
    }
}

/// Returns true if DWM currently reports the window as cloaked (by app, shell, or inherited).
pub fn is_window_cloaked(hwnd: HWND) -> bool {
    unsafe {
        let mut cloaked: u32 = 0;
        let ok = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut _ as *mut _,
            size_of::<u32>() as u32,
        );
        ok.is_ok() && cloaked != 0
    }
}
