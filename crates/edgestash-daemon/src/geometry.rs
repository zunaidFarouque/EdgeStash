use edgestash_ipc::types::{Rect, ScreenEdge};

use crate::dwm::BorderInsets;
use crate::platform::MonitorInfo;

const BASE_MIN_THICKNESS_96DPI: i32 = 240;

/// Base 96-DPI height of the slim EdgeStash drawer action strip docked above open stashed windows.
pub const DRAWER_HEADER_HEIGHT_PX: i32 = 28;

/// Interactive buttons positioned top-right in the slim drawer header strip (`[Close] [Unstash] [Pin]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderButtonKind {
    Close,
    Unstash,
    Pin,
}

/// Returns DPI-scaled header bar height (`28px` at 96 DPI).
pub fn drawer_header_height(dpi: u32) -> i32 {
    ((DRAWER_HEADER_HEIGHT_PX * (dpi.max(96) as i32)) / 96).max(DRAWER_HEADER_HEIGHT_PX)
}

/// Computes local `(0..width, 0..height)` hit-test rectangles for `[Close] [Unstash] [Pin]`
/// anchored in the top-right corner of the header bar (`Pin` at far right).
pub fn header_button_rects(width: i32, height: i32) -> [(HeaderButtonKind, Rect); 3] {
    let h = height.max(20);
    let btn_h = (h - 4).max(16);
    let top = (h - btn_h) / 2;
    let bottom = top + btn_h;
    let btn_w = (btn_h + 6).min((width / 4).max(18));
    let gap = 3;
    let margin_r = 4;

    let pin_right = (width - margin_r).max(btn_w);
    let pin_left = pin_right - btn_w;
    let unstash_right = pin_left - gap;
    let unstash_left = unstash_right - btn_w;
    let close_right = unstash_left - gap;
    let close_left = close_right - btn_w;

    [
        (HeaderButtonKind::Close, Rect::new(close_left, top, close_right, bottom)),
        (HeaderButtonKind::Unstash, Rect::new(unstash_left, top, unstash_right, bottom)),
        (HeaderButtonKind::Pin, Rect::new(pin_left, top, pin_right, bottom)),
    ]
}

/// Computes `(header_rect, window_rects)` for a single or grouped slot on `monitor`,
/// docking a slim header strip above the window(s) and constraining docked drawers inside `dock_reserved_rect`.
pub fn compute_slot_rects_on_monitor(
    monitor: &MonitorInfo,
    edge: ScreenEdge,
    thicknesses: &[i32],
    dock_reserved_rect: Option<Rect>,
) -> (Rect, Vec<Rect>) {
    let bounds = dock_reserved_rect.unwrap_or(monitor.work_rect);
    let h = drawer_header_height(monitor.dpi).min((bounds.height() - 48).max(0));
    if thicknesses.is_empty() {
        let hdr = Rect::new(bounds.left, bounds.top, bounds.right, bounds.top + h);
        return (hdr, Vec::new());
    }

    let scale = (monitor.dpi.max(96) as f32) / 96.0;
    let min_px = (BASE_MIN_THICKNESS_96DPI as f32 * scale).round() as i32;
    let clamp_thickness = |req: i32| -> i32 {
        if let Some(res) = dock_reserved_rect {
            let axis = if edge.is_vertical_edge() {
                res.width()
            } else {
                res.height()
            }
            .max(1);
            let eff_min = min_px.min(axis);
            req.clamp(eff_min, axis)
        } else {
            clamp_drawer_thickness(monitor, edge, req)
        }
    };

    let clamped: Vec<i32> = thicknesses.iter().map(|&t| clamp_thickness(t)).collect();
    let max_thick = clamped.iter().copied().max().unwrap_or(320);
    let count = clamped.len() as i32;

    match edge {
        ScreenEdge::Left | ScreenEdge::Right => {
            let hdr_top = bounds.top;
            let hdr_bottom = (hdr_top + h).min(bounds.bottom);
            let header_rect = if edge == ScreenEdge::Left {
                Rect::new(bounds.left, hdr_top, bounds.left + max_thick, hdr_bottom)
            } else {
                Rect::new(bounds.right - max_thick, hdr_top, bounds.right, hdr_bottom)
            };

            let avail_h = (bounds.bottom - hdr_bottom).max(0);
            let base = avail_h / count;
            let rem = avail_h % count;
            let mut cursor = hdr_bottom;
            let win_rects = clamped
                .into_iter()
                .enumerate()
                .map(|(i, thick)| {
                    let extra = if (i as i32) < rem { 1 } else { 0 };
                    let seg_len = base + extra;
                    let top = cursor;
                    let bottom = (cursor + seg_len).min(bounds.bottom);
                    cursor = bottom;
                    if edge == ScreenEdge::Left {
                        Rect::new(bounds.left, top, bounds.left + thick, bottom)
                    } else {
                        Rect::new(bounds.right - thick, top, bounds.right, bottom)
                    }
                })
                .collect();
            (header_rect, win_rects)
        }
        ScreenEdge::Top | ScreenEdge::Bottom => {
            let slot_top = if edge == ScreenEdge::Top {
                bounds.top
            } else {
                (bounds.bottom - max_thick).max(bounds.top)
            };
            let hdr_bottom = (slot_top + h).min(bounds.bottom);
            let header_rect = Rect::new(bounds.left, slot_top, bounds.right, hdr_bottom);

            let avail_w = bounds.width().max(0);
            let base = avail_w / count;
            let rem = avail_w % count;
            let mut cursor = bounds.left;
            let win_rects = clamped
                .into_iter()
                .enumerate()
                .map(|(i, thick)| {
                    let extra = if (i as i32) < rem { 1 } else { 0 };
                    let seg_len = base + extra;
                    let left = cursor;
                    let right = (cursor + seg_len).min(bounds.right);
                    cursor = right;
                    if edge == ScreenEdge::Top {
                        let bottom = (bounds.top + thick).max(hdr_bottom).min(bounds.bottom);
                        Rect::new(left, hdr_bottom, right, bottom)
                    } else {
                        let top = (bounds.bottom - thick).max(hdr_bottom);
                        Rect::new(left, top, right, bounds.bottom)
                    }
                })
                .collect();
            (header_rect, win_rects)
        }
    }
}

/// Converts a desired visible glass origin into the Win32 `SetWindowPos` origin
/// by compensating for Windows 10/11 invisible shadow border insets.
pub fn visible_to_window_origin(
    visible_x: i32,
    visible_y: i32,
    insets: BorderInsets,
) -> (i32, i32) {
    (visible_x - insets.left, visible_y - insets.top)
}

/// Converts a desired visible glass rectangle into a Win32 `SetWindowPos` rectangle.
pub fn visible_to_window_rect(visible: Rect, insets: BorderInsets) -> Rect {
    Rect::new(
        visible.left - insets.left,
        visible.top - insets.top,
        visible.right + insets.right,
        visible.bottom + insets.bottom,
    )
}

/// Clamps drawer thickness to `[240px * dpi_scale .. 75% of monitor axis]`.
pub fn clamp_drawer_thickness(monitor: &MonitorInfo, edge: ScreenEdge, requested_px: i32) -> i32 {
    let scale = (monitor.dpi.max(96) as f32) / 96.0;
    let min_px = (BASE_MIN_THICKNESS_96DPI as f32 * scale).round() as i32;
    let span = if edge.is_vertical_edge() {
        monitor.work_rect.width()
    } else {
        monitor.work_rect.height()
    };
    let max_px = ((span as f32) * 0.75).round() as i32;
    let effective_min = min_px.min(max_px.max(1));
    requested_px.clamp(effective_min, max_px.max(effective_min))
}

/// Computes the target visible drawer rectangle on `monitor` for `edge`.
/// When `segment` is `Some((start_along_edge, length_along_edge))`, places the window in that sub-segment.
pub fn compute_drawer_rect(
    monitor: &MonitorInfo,
    edge: ScreenEdge,
    requested_thickness_px: i32,
    segment: Option<(i32, i32)>,
) -> Rect {
    let thickness = clamp_drawer_thickness(monitor, edge, requested_thickness_px);
    let work = monitor.work_rect;

    match edge {
        ScreenEdge::Left | ScreenEdge::Right => {
            let (top, bottom) = match segment {
                Some((seg_top, seg_len)) => (seg_top, (seg_top + seg_len).min(work.bottom)),
                None => (work.top, work.bottom),
            };
            if edge == ScreenEdge::Left {
                Rect::new(work.left, top, work.left + thickness, bottom)
            } else {
                Rect::new(work.right - thickness, top, work.right, bottom)
            }
        }
        ScreenEdge::Top | ScreenEdge::Bottom => {
            let (left, right) = match segment {
                Some((seg_left, seg_len)) => (seg_left, (seg_left + seg_len).min(work.right)),
                None => (work.left, work.right),
            };
            if edge == ScreenEdge::Top {
                Rect::new(left, work.top, right, work.top + thickness)
            } else {
                Rect::new(left, work.bottom - thickness, right, work.bottom)
            }
        }
    }
}

/// Tiles `thicknesses` cleanly into non-overlapping sub-segments along `edge`.
pub fn compute_tiled_group_rects(
    monitor: &MonitorInfo,
    edge: ScreenEdge,
    thicknesses: &[i32],
) -> Vec<Rect> {
    if thicknesses.is_empty() {
        return Vec::new();
    }

    let count = thicknesses.len() as i32;
    let work = monitor.work_rect;
    let total_along_edge = if edge.is_vertical_edge() {
        work.height()
    } else {
        work.width()
    };
    let origin_along_edge = if edge.is_vertical_edge() {
        work.top
    } else {
        work.left
    };

    let base_seg = total_along_edge / count;
    let remainder = total_along_edge % count;

    let mut cursor = origin_along_edge;
    thicknesses
        .iter()
        .enumerate()
        .map(|(idx, &thick)| {
            let extra = if (idx as i32) < remainder { 1 } else { 0 };
            let seg_len = base_seg + extra;
            let rect = compute_drawer_rect(monitor, edge, thick, Some((cursor, seg_len)));
            cursor += seg_len;
            rect
        })
        .collect()
}

/// Computes the off-bezel visible origin for sliding into/out of `target_rect`.
pub fn compute_offbezel_origin(monitor: &MonitorInfo, edge: ScreenEdge, target_rect: Rect) -> (i32, i32) {
    let work = monitor.work_rect;
    match edge {
        ScreenEdge::Left => (work.left - target_rect.width(), target_rect.top),
        ScreenEdge::Right => (work.right, target_rect.top),
        ScreenEdge::Top => (target_rect.left, work.top - target_rect.height()),
        ScreenEdge::Bottom => (target_rect.left, work.bottom),
    }
}

/// Computes the combined bounding rectangle of all connected monitors.
pub fn virtual_desktop_bounds(monitors: &[MonitorInfo]) -> Rect {
    monitors
        .iter()
        .map(|m| m.rect)
        .reduce(Rect::union)
        .unwrap_or(Rect::new(0, 0, 1920, 1080))
}

/// Computes a safe dormant off-screen parking origin completely outside `virtual_bounds`.
pub fn compute_parked_rect(virtual_bounds: Rect, target_rect: Rect) -> (i32, i32) {
    let _ = target_rect;
    (virtual_bounds.right + 128, virtual_bounds.top)
}

/// Selects the best target monitor from `monitors`: prefers matching `id`, else primary, else first.
pub fn resolve_target_monitor<'a>(
    monitors: &'a [MonitorInfo],
    preferred_id: Option<&str>,
) -> Option<&'a MonitorInfo> {
    if let Some(id) = preferred_id
        && let Some(m) = monitors.iter().find(|m| m.id == id)
    {
        return Some(m);
    }
    monitors
        .iter()
        .find(|m| m.is_primary)
        .or_else(|| monitors.first())
}
