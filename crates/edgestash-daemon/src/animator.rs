use std::time::Instant;

use edgestash_ipc::types::AnimationStyle;
use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Gdi::{
    RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, RDW_UPDATENOW, RedrawWindow,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetClassNameW, GetWindowLongPtrW, GetWindowRect, HWND_TOPMOST, IsIconic,
    LWA_ALPHA, SW_RESTORE, SWP_ASYNCWINDOWPOS, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOCOPYBITS,
    SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetLayeredWindowAttributes, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, WS_EX_LAYERED,
};

use crate::dwm::{BorderInsets, get_invisible_border_insets, set_window_cloaked};
use crate::geometry::visible_to_window_origin;
use crate::platform::win32::bring_drag_image_to_top;

/// Single interpolated frame pose computed for an animation step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FramePose {
    pub x: i32,
    pub y: i32,
    pub alpha: Option<u8>,
}

/// Cubic ease-out curve (`1 - (1 - t)^3`).
#[inline]
pub fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

/// Computes a single window pose for normalized progress `raw_t` in `[0.0, 1.0]`.
pub fn compute_animation_frame(
    start_x: i32,
    start_y: i32,
    end_x: i32,
    end_y: i32,
    raw_t: f32,
    style: AnimationStyle,
    is_opening: bool,
) -> FramePose {
    let clamped_t = raw_t.clamp(0.0, 1.0);
    let eased_t = ease_out_cubic(clamped_t);

    match style {
        AnimationStyle::Instant => FramePose {
            x: end_x,
            y: end_y,
            alpha: None,
        },
        AnimationStyle::Slide => FramePose {
            x: start_x + ((end_x - start_x) as f32 * eased_t).round() as i32,
            y: start_y + ((end_y - start_y) as f32 * eased_t).round() as i32,
            alpha: None,
        },
        AnimationStyle::FloatAndFade => {
            // Float only 16px along the movement vector while fading 0..255
            let dx = (end_x - start_x).signum() * 16;
            let dy = (end_y - start_y).signum() * 16;
            let (float_start_x, float_start_y, float_end_x, float_end_y) = if is_opening {
                (end_x - dx, end_y - dy, end_x, end_y)
            } else {
                (start_x, start_y, start_x + dx, start_y + dy)
            };

            let cur_x =
                float_start_x + ((float_end_x - float_start_x) as f32 * eased_t).round() as i32;
            let cur_y =
                float_start_y + ((float_end_y - float_start_y) as f32 * eased_t).round() as i32;
            let alpha = if is_opening {
                (255.0 * eased_t).round() as u8
            } else {
                (255.0 * (1.0 - eased_t)).round() as u8
            };

            FramePose {
                x: cur_x,
                y: cur_y,
                alpha: Some(alpha),
            }
        }
    }
}

/// Parameters for animating one window in a synchronized `DwmFlush()` batch.
#[derive(Debug, Clone, Copy)]
pub struct Win32TransitionTarget {
    pub hwnd: HWND,
    pub start_visible_x: i32,
    pub start_visible_y: i32,
    pub end_visible_x: i32,
    pub end_visible_y: i32,
}

/// Animates one or more stashed windows into or out of view in lockstep with `DwmFlush()`,
/// compensating for Win11 invisible borders and preserving `SWP_NOSIZE`.
pub fn animate_windows_transition(
    targets: &[Win32TransitionTarget],
    duration_ms: u32,
    style: AnimationStyle,
    is_opening: bool,
) {
    if targets.is_empty() {
        return;
    }

    struct PreparedTarget {
        hwnd: HWND,
        start_x: i32,
        start_y: i32,
        end_x: i32,
        end_y: i32,
        orig_ex_style: isize,
        had_layered: bool,
    }

    let prepared: Vec<PreparedTarget> = targets
        .iter()
        .map(|t| {
            let insets: BorderInsets = get_invisible_border_insets(t.hwnd).unwrap_or_default();
            let (start_x, start_y) =
                visible_to_window_origin(t.start_visible_x, t.start_visible_y, insets);
            let (end_x, end_y) = visible_to_window_origin(t.end_visible_x, t.end_visible_y, insets);
            let orig_ex_style = unsafe { GetWindowLongPtrW(t.hwnd, GWL_EXSTYLE) };
            let had_layered = (orig_ex_style & (WS_EX_LAYERED.0 as isize)) != 0;
            PreparedTarget {
                hwnd: t.hwnd,
                start_x,
                start_y,
                end_x,
                end_y,
                orig_ex_style,
                had_layered,
            }
        })
        .collect();

    unsafe {
        if is_opening {
            for p in &prepared {
                if IsIconic(p.hwnd).as_bool() {
                    let _ = ShowWindow(p.hwnd, SW_RESTORE);
                }
                // Uncloak before first frame
                let _ = set_window_cloaked(p.hwnd, false);
            }
        }

        if style == AnimationStyle::Instant || duration_ms == 0 {
            for p in &prepared {
                let _ = SetWindowPos(
                    p.hwnd,
                    HWND_TOPMOST,
                    p.end_x,
                    p.end_y,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOCOPYBITS | SWP_FRAMECHANGED,
                );
                if !is_opening {
                    let _ = set_window_cloaked(p.hwnd, true);
                } else {
                    refresh_anim_target(p.hwnd);
                }
            }
            if is_opening {
                bring_drag_image_to_top();
            }
            return;
        }

        if style == AnimationStyle::FloatAndFade {
            for p in &prepared {
                if !p.had_layered {
                    SetWindowLongPtrW(
                        p.hwnd,
                        GWL_EXSTYLE,
                        p.orig_ex_style | (WS_EX_LAYERED.0 as isize),
                    );
                }
            }
        }

        let start_time = Instant::now();
        loop {
            let elapsed = start_time.elapsed().as_secs_f32() * 1000.0;
            let raw_t = (elapsed / duration_ms as f32).clamp(0.0, 1.0);

            for p in &prepared {
                let pose = compute_animation_frame(
                    p.start_x, p.start_y, p.end_x, p.end_y, raw_t, style, is_opening,
                );
                if let Some(alpha) = pose.alpha {
                    let _ = SetLayeredWindowAttributes(p.hwnd, COLORREF(0), alpha, LWA_ALPHA);
                }
                let _ = SetWindowPos(
                    p.hwnd,
                    HWND_TOPMOST,
                    pose.x,
                    pose.y,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
                );
            }

            if raw_t >= 1.0 {
                break;
            }

            // Lock loop iteration to exact DWM VSync presentation
            let _ = DwmFlush();
        }

        if is_opening {
            for p in &prepared {
                let _ = SetWindowPos(
                    p.hwnd,
                    HWND_TOPMOST,
                    p.end_x,
                    p.end_y,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                );
                refresh_anim_target(p.hwnd);
            }
            bring_drag_image_to_top();
        } else {
            for p in &prepared {
                // Cloak first while alpha is 0 / off-bezel so removing WS_EX_LAYERED never flashes!
                let _ = set_window_cloaked(p.hwnd, true);
                let _ = SetWindowPos(
                    p.hwnd,
                    HWND_TOPMOST,
                    p.end_x,
                    p.end_y,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
                );
            }
        }

        // Clean up WS_EX_LAYERED after transition so hardware-accelerated apps run uninhibited
        if style == AnimationStyle::FloatAndFade {
            for p in &prepared {
                if !p.had_layered {
                    let _ = SetLayeredWindowAttributes(p.hwnd, COLORREF(0), 255, LWA_ALPHA);
                    SetWindowLongPtrW(p.hwnd, GWL_EXSTYLE, p.orig_ex_style);
                    let _ = SetWindowPos(
                        p.hwnd,
                        None,
                        0,
                        0,
                        0,
                        0,
                        SWP_NOSIZE | SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                    );
                }
            }
        }
    }
}

fn refresh_anim_target(hwnd: HWND) {
    unsafe {
        let _ = RedrawWindow(
            hwnd,
            None,
            None,
            RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_UPDATENOW | RDW_FRAME | RDW_ERASE,
        );

        let mut class_buf = [0u16; 64];
        let len = GetClassNameW(hwnd, &mut class_buf);
        if len > 0 {
            let class_name = String::from_utf16_lossy(&class_buf[..len as usize]);
            if class_name == "CabinetWClass" || class_name == "ExploreWClass" {
                let mut r = RECT::default();
                if GetWindowRect(hwnd, &mut r).is_ok() {
                    let w = r.right - r.left;
                    let h = r.bottom - r.top;
                    if w > 2 && h > 2 {
                        let _ = SetWindowPos(
                            hwnd,
                            None,
                            r.left,
                            r.top,
                            w + 1,
                            h,
                            SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOCOPYBITS | SWP_FRAMECHANGED,
                        );
                        let _ = SetWindowPos(
                            hwnd,
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

/// Convenience wrapper matching Section 4.4's single-window signature.
#[allow(clippy::too_many_arguments)]
pub fn animate_window_transition(
    hwnd: HWND,
    start_visible_x: i32,
    start_visible_y: i32,
    end_visible_x: i32,
    end_visible_y: i32,
    duration_ms: u32,
    style: AnimationStyle,
    is_opening: bool,
) {
    animate_windows_transition(
        &[Win32TransitionTarget {
            hwnd,
            start_visible_x,
            start_visible_y,
            end_visible_x,
            end_visible_y,
        }],
        duration_ms,
        style,
        is_opening,
    );
}
