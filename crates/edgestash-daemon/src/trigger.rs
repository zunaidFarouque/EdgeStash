use edgestash_ipc::config::{AreaAwareScrollMode, GestureMode};
use edgestash_ipc::types::{ChurnZoneId, EdgePillStyle, Rect, ScreenEdge};

use crate::platform::{MonitorInfo, WindowHandle};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharmsPhase {
    Idle,
    ArmedInCorner {
        anchor_along_bezel: i32,
        entered_at_ms: u64,
    },
    TriggeredCooldown,
}

/// Windows 8 Charms-style corner-then-bezel-slide state machine preventing accidental corner triggers.
#[derive(Debug, Clone)]
pub struct CharmsStateMachine {
    pub corner_size_px: i32,
    pub slide_distance_px: i32,
    pub timeout_ms: u64,
    phase: CharmsPhase,
}

impl CharmsStateMachine {
    pub fn new(corner_size_px: i32, slide_distance_px: i32, timeout_ms: u64) -> Self {
        Self {
            corner_size_px: corner_size_px.max(4),
            slide_distance_px: slide_distance_px.max(8),
            timeout_ms: timeout_ms.max(100),
            phase: CharmsPhase::Idle,
        }
    }

    pub fn reset(&mut self) {
        self.phase = CharmsPhase::Idle;
    }

    pub fn is_armed(&self) -> bool {
        matches!(self.phase, CharmsPhase::ArmedInCorner { .. })
    }

    /// Feeds cursor coordinates `(x, y)` at `now_ms` for `(monitor, edge)`.
    /// Returns `true` once when the corner-slide gesture completes within `timeout_ms`.
    pub fn update(
        &mut self,
        monitor: &MonitorInfo,
        edge: ScreenEdge,
        cursor: (i32, i32),
        now_ms: u64,
    ) -> bool {
        let rect = monitor.rect;
        let in_bezel = is_in_bezel_corridor(rect, edge, self.corner_size_px, cursor);
        let in_corner = is_in_edge_corner(rect, edge, self.corner_size_px, cursor);
        let along_bezel = if edge.is_vertical_edge() {
            cursor.1
        } else {
            cursor.0
        };

        if !in_bezel {
            self.phase = CharmsPhase::Idle;
            return false;
        }

        match self.phase {
            CharmsPhase::Idle => {
                if in_corner {
                    self.phase = CharmsPhase::ArmedInCorner {
                        anchor_along_bezel: along_bezel,
                        entered_at_ms: now_ms,
                    };
                }
                false
            }
            CharmsPhase::ArmedInCorner {
                anchor_along_bezel,
                entered_at_ms,
            } => {
                if now_ms.saturating_sub(entered_at_ms) > self.timeout_ms {
                    self.phase = CharmsPhase::Idle;
                    return false;
                }
                if (along_bezel - anchor_along_bezel).abs() >= self.slide_distance_px {
                    self.phase = CharmsPhase::TriggeredCooldown;
                    return true;
                }
                false
            }
            CharmsPhase::TriggeredCooldown => false,
        }
    }
}

fn is_in_bezel_corridor(
    rect: Rect,
    edge: ScreenEdge,
    thickness_px: i32,
    cursor: (i32, i32),
) -> bool {
    let (x, y) = cursor;
    let b = 16; // 16px outer bezel overshoot tolerance for physical display edges / cursor pushing
    match edge {
        ScreenEdge::Left => {
            x >= rect.left - b
                && x <= rect.left + thickness_px
                && y >= rect.top - b
                && y <= rect.bottom + b
        }
        ScreenEdge::Right => {
            x >= rect.right - thickness_px
                && x <= rect.right + b
                && y >= rect.top - b
                && y <= rect.bottom + b
        }
        ScreenEdge::Top => {
            y >= rect.top - b
                && y <= rect.top + thickness_px
                && x >= rect.left - b
                && x <= rect.right + b
        }
        ScreenEdge::Bottom => {
            y >= rect.bottom - thickness_px
                && y <= rect.bottom + b
                && x >= rect.left - b
                && x <= rect.right + b
        }
    }
}

/// Computes the 1px-thin topmost trigger strip rectangle for `(monitor, edge)`.
/// Extends 2px outside the physical monitor boundary into outer overshoot space
/// so pushing against or slightly past the screen edge is reliably captured.
pub fn trigger_strip_rect(mon: &MonitorInfo, edge: ScreenEdge, thickness_px: i32) -> Rect {
    let r = mon.rect;
    let t = thickness_px.max(1);
    let out = 2;
    match edge {
        ScreenEdge::Left => Rect::new(r.left - out, r.top, r.left + t, r.bottom),
        ScreenEdge::Right => Rect::new(r.right - t, r.top, r.right + out, r.bottom),
        ScreenEdge::Top => Rect::new(r.left, r.top - out, r.right, r.top + t),
        ScreenEdge::Bottom => Rect::new(r.left, r.bottom - t, r.right, r.bottom + out),
    }
}

/// Computes the translucent visual pill rectangle along the monitor work-area edge, scaled by monitor DPI.
pub fn edge_pill_rect(mon: &MonitorInfo, edge: ScreenEdge) -> Rect {
    edge_pill_rect_for_style(mon, edge, EdgePillStyle::MinimalLine, 1)
}

/// Computes the visual pill rectangle sized dynamically for `style` (`MinimalLine` vs `AppIconsStack`) and `slot_count`.
pub fn edge_pill_rect_for_style(
    mon: &MonitorInfo,
    edge: ScreenEdge,
    style: EdgePillStyle,
    slot_count: usize,
) -> Rect {
    let w = mon.work_rect;
    let dpi_scale = mon.dpi.max(96) as i32;
    let (thickness, span) = match style {
        EdgePillStyle::AppIconsStack => {
            let count = slot_count.clamp(1, 8) as i32;
            let thick = ((28 * dpi_scale) / 96).max(28);
            let slot_step = ((24 * dpi_scale) / 96).max(24);
            let pad = ((4 * dpi_scale) / 96).max(4);
            let max_axis = if edge.is_vertical_edge() {
                (w.height() - 16).max(thick)
            } else {
                (w.width() - 16).max(thick)
            };
            (thick, (count * slot_step + 2 * pad).min(max_axis))
        }
        EdgePillStyle::MinimalLine | EdgePillStyle::Hidden => (
            ((10 * dpi_scale) / 96).max(10),
            ((64 * dpi_scale) / 96).max(64),
        ),
    };
    match edge {
        ScreenEdge::Left => {
            let cy = w.top + (w.height() - span) / 2;
            Rect::new(w.left, cy, w.left + thickness, cy + span)
        }
        ScreenEdge::Right => {
            let cy = w.top + (w.height() - span) / 2;
            Rect::new(w.right - thickness, cy, w.right, cy + span)
        }
        ScreenEdge::Top => {
            let cx = w.left + (w.width() - span) / 2;
            Rect::new(cx, w.top, cx + span, w.top + thickness)
        }
        ScreenEdge::Bottom => {
            let cx = w.left + (w.width() - span) / 2;
            Rect::new(cx, w.bottom - thickness, cx + span, w.bottom)
        }
    }
}

/// Maps a local `(client_x, client_y)` click inside an `AppIconsStack` pill to the clicked slot index.
pub fn hit_test_pill_slot_index(
    pill_rect: Rect,
    edge: ScreenEdge,
    style: EdgePillStyle,
    slot_count: usize,
    dpi: u32,
    client_pt: (i32, i32),
) -> Option<usize> {
    if style != EdgePillStyle::AppIconsStack || slot_count == 0 {
        return None;
    }
    let w = pill_rect.width();
    let h = pill_rect.height();
    let (cx, cy) = client_pt;
    if cx < 0 || cx >= w || cy < 0 || cy >= h {
        return None;
    }
    let count = slot_count.min(8);
    let pad = ((4 * dpi.max(96) as i32) / 96).max(4);
    let axis_coord = if edge.is_vertical_edge() { cy } else { cx };
    let total_inner = (if edge.is_vertical_edge() { h } else { w } - 2 * pad).max(1);
    let rel = (axis_coord - pad).clamp(0, total_inner - 1);
    let idx = (rel * count as i32) / total_inner;
    Some((idx as usize).min(count - 1))
}

/// Returns all monitor edges whose Charms corner zone currently contains `cursor`,
/// resolving shared corner pixels (e.g. top-right touching both `Top` and `Right`).
pub fn corner_candidate_edges(
    mon: &MonitorInfo,
    corner_size_px: i32,
    cursor: (i32, i32),
) -> Vec<ScreenEdge> {
    let c = corner_size_px.max(4);
    ScreenEdge::ALL
        .into_iter()
        .filter(|&edge| is_in_edge_corner(mon.rect, edge, c, cursor))
        .collect()
}

/// Returns whether hovering over the visual edge pill opens the drawer in `mode`.
pub fn allows_pill_hover(mode: GestureMode) -> bool {
    matches!(
        mode,
        GestureMode::EdgeBump | GestureMode::CornerSlide | GestureMode::EdgeHover
    )
}

/// Returns whether hovering anywhere on the 1px trigger strip opens the drawer in `mode`.
pub fn allows_strip_hover(mode: GestureMode) -> bool {
    matches!(mode, GestureMode::EdgeHover)
}

/// Returns whether left-clicking the visual edge pill toggles the drawer in `mode`.
pub fn allows_pill_click(mode: GestureMode) -> bool {
    !matches!(mode, GestureMode::HotkeyOnly)
}

fn is_in_edge_corner(rect: Rect, edge: ScreenEdge, c: i32, cursor: (i32, i32)) -> bool {
    if !is_in_bezel_corridor(rect, edge, c, cursor) {
        return false;
    }
    let (x, y) = cursor;
    let b = 4;
    match edge {
        ScreenEdge::Left | ScreenEdge::Right => {
            (y >= rect.top - b && y <= rect.top + c)
                || (y >= rect.bottom - c && y <= rect.bottom + b)
        }
        ScreenEdge::Top | ScreenEdge::Bottom => {
            (x >= rect.left - b && x <= rect.left + c)
                || (x >= rect.right - c && x <= rect.right + b)
        }
    }
}

/// Evaluates whether `cursor` lies within an angled corridor that widens inward from the monitor
/// bezel by `diagonal_angle_deg` as the user slides further from `anchor_along_bezel`.
/// Outward physical pushing into the screen boundary is always permitted.
pub fn is_in_angled_corridor(
    rect: Rect,
    zone: ChurnZoneId,
    base_corridor_px: i32,
    diagonal_angle_deg: i32,
    slide_distance_px: i32,
    anchor_along_bezel: i32,
    cursor: (i32, i32),
) -> bool {
    let edge = zone.edge();
    let (x, y) = cursor;
    let b = 16; // 16px outer bezel overshoot tolerance for physical pushing

    // Check along-bezel boundary
    let (along_bezel, in_bezel_extent) = if edge.is_vertical_edge() {
        (y, y >= rect.top - b && y <= rect.bottom + b)
    } else {
        (x, x >= rect.left - b && x <= rect.right + b)
    };
    if !in_bezel_extent {
        return false;
    }

    let signed_delta = (along_bezel - anchor_along_bezel) * zone.slide_sign();
    let effective_d = signed_delta.max(0).min(slide_distance_px.max(16));
    let angle_clamped = diagonal_angle_deg.clamp(15, 75);
    let rad = (angle_clamped as f32).to_radians();
    let inward_expansion = (effective_d as f32 * rad.tan()).round() as i32;
    let max_inward = base_corridor_px.max(24) + inward_expansion;

    match edge {
        ScreenEdge::Left => x >= rect.left - b && x <= rect.left + max_inward,
        ScreenEdge::Right => x >= rect.right - max_inward && x <= rect.right + b,
        ScreenEdge::Top => y >= rect.top - b && y <= rect.top + max_inward,
        ScreenEdge::Bottom => y >= rect.bottom - max_inward && y <= rect.bottom + b,
    }
}

/// Computes the visualizer preview overlay rectangle for a given churn zone on `mon`.
pub fn compute_churn_preview_rect(
    mon: &MonitorInfo,
    zone: ChurnZoneId,
    corner_box_px: i32,
    slide_distance_px: i32,
    diagonal_angle_deg: i32,
) -> Rect {
    let c = corner_box_px.max(DirectedZoneMachine::CORNER_HIT_MIN_PX);
    let s = slide_distance_px.max(20);
    let angle_clamped = diagonal_angle_deg.clamp(15, 75);
    let rad = (angle_clamped as f32).to_radians();
    let max_inward = c + (s as f32 * rad.tan()).round() as i32;
    let extent_along = s + c + 40;
    let extent_across = max_inward + 40;
    let r = mon.rect;

    match zone {
        ChurnZoneId::TopRightDown => Rect::new(
            r.right - extent_across,
            r.top,
            r.right,
            (r.top + extent_along).min(r.bottom),
        ),
        ChurnZoneId::BottomRightUp => Rect::new(
            r.right - extent_across,
            (r.bottom - extent_along).max(r.top),
            r.right,
            r.bottom,
        ),
        ChurnZoneId::TopLeftDown => Rect::new(
            r.left,
            r.top,
            r.left + extent_across,
            (r.top + extent_along).min(r.bottom),
        ),
        ChurnZoneId::BottomLeftUp => Rect::new(
            r.left,
            (r.bottom - extent_along).max(r.top),
            r.left + extent_across,
            r.bottom,
        ),
        ChurnZoneId::TopLeftRight => Rect::new(
            r.left,
            r.top,
            (r.left + extent_along).min(r.right),
            r.top + extent_across,
        ),
        ChurnZoneId::TopRightLeft => Rect::new(
            (r.right - extent_along).max(r.left),
            r.top,
            r.right,
            r.top + extent_across,
        ),
        ChurnZoneId::BottomLeftRight => Rect::new(
            r.left,
            (r.bottom - extent_across).max(r.top),
            (r.left + extent_along).min(r.right),
            r.bottom,
        ),
        ChurnZoneId::BottomRightLeft => Rect::new(
            (r.right - extent_along).max(r.left),
            (r.bottom - extent_across).max(r.top),
            r.right,
            r.bottom,
        ),
    }
}

fn is_in_zone_corner(rect: Rect, zone: ChurnZoneId, c: i32, cursor: (i32, i32)) -> bool {
    let edge = zone.edge();
    let hit_size = c.max(DirectedZoneMachine::CORNER_HIT_MIN_PX);
    let (x, y) = cursor;
    let far = zone.is_far_corner();
    let b = 16; // 16px outer bezel tolerance
    if edge.is_vertical_edge() {
        let in_x = match edge {
            ScreenEdge::Left => x >= rect.left - b && x <= rect.left + hit_size,
            ScreenEdge::Right => x >= rect.right - hit_size && x <= rect.right + b,
            _ => false,
        };
        let in_y = if far {
            y >= rect.bottom - hit_size && y <= rect.bottom + b
        } else {
            y >= rect.top - b && y <= rect.top + hit_size
        };
        in_x && in_y
    } else {
        let in_y = match edge {
            ScreenEdge::Top => y >= rect.top - b && y <= rect.top + hit_size,
            ScreenEdge::Bottom => y >= rect.bottom - hit_size && y <= rect.bottom + b,
            _ => false,
        };
        let in_x = if far {
            x >= rect.right - hit_size && x <= rect.right + b
        } else {
            x >= rect.left - b && x <= rect.left + hit_size
        };
        in_x && in_y
    }
}

/// Directed single-zone Charms state machine tracking entry into one specific corner and
/// directional slide along its monitor bezel corridor.
#[derive(Debug, Clone)]
pub struct DirectedZoneMachine {
    pub corner_size_px: i32,
    pub slide_distance_px: i32,
    pub timeout_ms: u64,
    pub diagonal_angle_deg: i32,
    last_slide_delta: i32,
    phase: CharmsPhase,
}

impl DirectedZoneMachine {
    pub const CORNER_HIT_MIN_PX: i32 = 20;
    pub const CORRIDOR_MIN_PX: i32 = 32;
    pub const CORRIDOR_MAX_PX: i32 = 36;

    pub fn new(corner_size_px: i32, slide_distance_px: i32, timeout_ms: u64) -> Self {
        Self::with_angle(corner_size_px, slide_distance_px, timeout_ms, 45)
    }

    pub fn with_angle(
        corner_size_px: i32,
        slide_distance_px: i32,
        timeout_ms: u64,
        diagonal_angle_deg: i32,
    ) -> Self {
        Self {
            corner_size_px: corner_size_px.max(4),
            slide_distance_px: slide_distance_px.max(8),
            timeout_ms: timeout_ms.max(100),
            diagonal_angle_deg: diagonal_angle_deg.clamp(15, 75),
            last_slide_delta: 0,
            phase: CharmsPhase::Idle,
        }
    }

    pub fn last_slide_delta(&self) -> i32 {
        self.last_slide_delta
    }

    pub fn is_armed(&self) -> bool {
        matches!(self.phase, CharmsPhase::ArmedInCorner { .. })
    }

    pub fn reset(&mut self) {
        self.phase = CharmsPhase::Idle;
        self.last_slide_delta = 0;
    }

    pub fn update(
        &mut self,
        monitor: &MonitorInfo,
        zone: ChurnZoneId,
        cursor: (i32, i32),
        now_ms: u64,
    ) -> bool {
        let rect = monitor.rect;
        let edge = zone.edge();
        let slide_corridor_px =
            (self.corner_size_px * 4).clamp(Self::CORRIDOR_MIN_PX, Self::CORRIDOR_MAX_PX);
        let along_bezel = if edge.is_vertical_edge() {
            cursor.1
        } else {
            cursor.0
        };

        match self.phase {
            CharmsPhase::Idle => {
                let in_corner = is_in_zone_corner(rect, zone, self.corner_size_px, cursor);
                if in_corner {
                    let corner_datum = match zone {
                        ChurnZoneId::TopRightDown | ChurnZoneId::TopLeftDown => rect.top,
                        ChurnZoneId::BottomRightUp | ChurnZoneId::BottomLeftUp => rect.bottom,
                        ChurnZoneId::TopLeftRight | ChurnZoneId::BottomLeftRight => rect.left,
                        ChurnZoneId::TopRightLeft | ChurnZoneId::BottomRightLeft => rect.right,
                    };
                    self.phase = CharmsPhase::ArmedInCorner {
                        anchor_along_bezel: corner_datum,
                        entered_at_ms: now_ms,
                    };
                }
                false
            }
            CharmsPhase::ArmedInCorner {
                anchor_along_bezel,
                mut entered_at_ms,
            } => {
                let in_corner = is_in_zone_corner(rect, zone, self.corner_size_px, cursor);
                let in_corridor = is_in_angled_corridor(
                    rect,
                    zone,
                    slide_corridor_px,
                    self.diagonal_angle_deg,
                    self.slide_distance_px,
                    anchor_along_bezel,
                    cursor,
                );
                if in_corner {
                    entered_at_ms = now_ms;
                    self.phase = CharmsPhase::ArmedInCorner {
                        anchor_along_bezel,
                        entered_at_ms,
                    };
                }
                if !in_corridor || now_ms.saturating_sub(entered_at_ms) > self.timeout_ms {
                    self.phase = CharmsPhase::Idle;
                    return false;
                }
                let signed_delta = (along_bezel - anchor_along_bezel) * zone.slide_sign();
                if signed_delta >= self.slide_distance_px {
                    self.last_slide_delta = signed_delta;
                    self.phase = CharmsPhase::TriggeredCooldown;
                    return true;
                }
                false
            }
            CharmsPhase::TriggeredCooldown => {
                let in_corner = is_in_zone_corner(rect, zone, self.corner_size_px, cursor);
                let in_corridor = is_in_bezel_corridor(rect, edge, slide_corridor_px * 2, cursor);
                if !in_corridor || in_corner {
                    self.phase = CharmsPhase::Idle;
                    if in_corner {
                        let corner_datum = match zone {
                            ChurnZoneId::TopRightDown | ChurnZoneId::TopLeftDown => rect.top,
                            ChurnZoneId::BottomRightUp | ChurnZoneId::BottomLeftUp => rect.bottom,
                            ChurnZoneId::TopLeftRight | ChurnZoneId::BottomLeftRight => rect.left,
                            ChurnZoneId::TopRightLeft | ChurnZoneId::BottomRightLeft => rect.right,
                        };
                        self.phase = CharmsPhase::ArmedInCorner {
                            anchor_along_bezel: corner_datum,
                            entered_at_ms: now_ms,
                        };
                    }
                }
                false
            }
        }
    }
}

/// Coordinates all 8 directed corner-slide ("churn") gestures for a monitor cleanly.
#[derive(Debug, Clone)]
pub struct DirectedCharmsDetector {
    zones: [DirectedZoneMachine; 8],
}

impl DirectedCharmsDetector {
    pub fn new(corner_size_px: i32, slide_distance_px: i32, timeout_ms: u64) -> Self {
        Self::with_angle(corner_size_px, slide_distance_px, timeout_ms, 45)
    }

    pub fn with_angle(
        corner_size_px: i32,
        slide_distance_px: i32,
        timeout_ms: u64,
        diagonal_angle_deg: i32,
    ) -> Self {
        Self {
            zones: std::array::from_fn(|_| {
                DirectedZoneMachine::with_angle(
                    corner_size_px,
                    slide_distance_px,
                    timeout_ms,
                    diagonal_angle_deg,
                )
            }),
        }
    }

    pub fn sync_params(&mut self, corner_size_px: i32, slide_distance_px: i32, timeout_ms: u64) {
        self.sync_params_dpi(corner_size_px, slide_distance_px, timeout_ms, 45, 96);
    }

    pub fn sync_params_dpi(
        &mut self,
        corner_size_px: i32,
        slide_distance_px: i32,
        timeout_ms: u64,
        diagonal_angle_deg: i32,
        dpi: u32,
    ) {
        let scale = (dpi.max(96) as f32) / 96.0;
        let c = ((corner_size_px.max(4) as f32) * scale).round() as i32;
        let s = ((slide_distance_px.max(8) as f32) * scale).round() as i32;
        let t = timeout_ms.max(100);
        let ang = diagonal_angle_deg.clamp(15, 75);
        for z in &mut self.zones {
            z.corner_size_px = c;
            z.slide_distance_px = s;
            z.timeout_ms = t;
            z.diagonal_angle_deg = ang;
        }
    }

    pub fn reset(&mut self) {
        for z in &mut self.zones {
            z.reset();
        }
    }

    pub fn is_any_armed(&self) -> bool {
        self.zones.iter().any(DirectedZoneMachine::is_armed)
    }

    /// Feeds cursor coordinates `(x, y)` and returns any newly completed [`ChurnZoneId`] gestures.
    pub fn update(
        &mut self,
        monitor: &MonitorInfo,
        cursor: (i32, i32),
        now_ms: u64,
    ) -> Vec<ChurnZoneId> {
        let mut fired = Vec::new();
        for (idx, zone) in ChurnZoneId::ALL.into_iter().enumerate() {
            if self.zones[idx].update(monitor, zone, cursor, now_ms) {
                fired.push(zone);
            }
        }
        if fired.len() > 1 {
            // Disambiguate shared-corner simultaneous triggers by selecting the zone
            // whose along-bezel slide delta was dominant.
            fired.sort_by_key(|&z| {
                let idx = ChurnZoneId::ALL.iter().position(|&x| x == z).unwrap_or(0);
                std::cmp::Reverse(self.zones[idx].last_slide_delta())
            });
            fired.truncate(1);
        }
        fired
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HoverPhase {
    Outside,
    Dwelling { entered_at_ms: u64 },
    Fired,
}

/// Dwell-timed 1px edge hover trigger state machine.
#[derive(Debug, Clone)]
pub struct EdgeHoverStateMachine {
    pub dwell_ms: u64,
    phase: HoverPhase,
}

impl EdgeHoverStateMachine {
    pub fn new(dwell_ms: u64) -> Self {
        Self {
            dwell_ms,
            phase: HoverPhase::Outside,
        }
    }

    pub fn reset(&mut self) {
        self.phase = HoverPhase::Outside;
    }

    /// Updates hover state given whether cursor is currently inside the 1px strip.
    /// Returns `true` once when dwell duration completes.
    pub fn update(&mut self, is_inside_strip: bool, now_ms: u64) -> bool {
        if !is_inside_strip {
            self.phase = HoverPhase::Outside;
            return false;
        }

        match self.phase {
            HoverPhase::Outside => {
                if self.dwell_ms == 0 {
                    self.phase = HoverPhase::Fired;
                    return true;
                }
                self.phase = HoverPhase::Dwelling {
                    entered_at_ms: now_ms,
                };
                false
            }
            HoverPhase::Dwelling { entered_at_ms } => {
                if now_ms.saturating_sub(entered_at_ms) >= self.dwell_ms {
                    self.phase = HoverPhase::Fired;
                    return true;
                }
                false
            }
            HoverPhase::Fired => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BumpPhase {
    Outside,
    Dwelling { entered_at_ms: u64 },
    TriggeredCooldown,
}

/// Crisp bezel-bump trigger state machine tracking cursor contact with the monitor's outer bezel.
/// Arms on bezel contact (0..=4px initial hit) and maintains dwell while pushed within the 12px push band,
/// making bezel pushes completely immune to micro-motion sensor jitter while sliding or stationary.
#[derive(Debug, Clone)]
pub struct EdgeBumpStateMachine {
    pub dwell_ms: u64,
    phase: BumpPhase,
}

impl EdgeBumpStateMachine {
    pub const INITIAL_HIT_ZONE_PX: i32 = 4;
    pub const PUSH_BAND_PX: i32 = 12;

    pub fn new(dwell_ms: u64) -> Self {
        Self {
            dwell_ms,
            phase: BumpPhase::Outside,
        }
    }

    pub fn reset(&mut self) {
        self.phase = BumpPhase::Outside;
    }

    pub fn is_armed(&self) -> bool {
        matches!(self.phase, BumpPhase::Dwelling { .. })
    }

    /// Updates state given cursor position on `(monitor, edge)`.
    /// Bezel contact is detected within 4px of monitor border, and dwell is maintained
    /// while the cursor is actively pushed in the 12px bezel push band (absorbing micro-motion jitter).
    pub fn update(
        &mut self,
        monitor: &MonitorInfo,
        edge: ScreenEdge,
        cursor: (i32, i32),
        now_ms: u64,
    ) -> bool {
        let in_hit_zone =
            is_in_bezel_corridor(monitor.rect, edge, Self::INITIAL_HIT_ZONE_PX, cursor);
        let in_push_band = is_in_bezel_corridor(monitor.rect, edge, Self::PUSH_BAND_PX, cursor);

        match self.phase {
            BumpPhase::Outside => {
                if in_hit_zone {
                    if self.dwell_ms == 0 {
                        self.phase = BumpPhase::TriggeredCooldown;
                        return true;
                    }
                    self.phase = BumpPhase::Dwelling {
                        entered_at_ms: now_ms,
                    };
                }
                false
            }
            BumpPhase::Dwelling { entered_at_ms } => {
                if in_push_band {
                    if now_ms.saturating_sub(entered_at_ms) >= self.dwell_ms {
                        self.phase = BumpPhase::TriggeredCooldown;
                        return true;
                    }
                    false
                } else {
                    self.phase = BumpPhase::Outside;
                    false
                }
            }
            BumpPhase::TriggeredCooldown => {
                if !in_push_band {
                    self.phase = BumpPhase::Outside;
                }
                false
            }
        }
    }
}

/// Target destination when dropping a dragged window into a screen drop zone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DragDropTarget {
    Edge {
        monitor_id: String,
        edge: ScreenEdge,
    },
    Corner {
        monitor_id: String,
        zone: ChurnZoneId,
    },
}

impl DragDropTarget {
    pub fn monitor_id(&self) -> &str {
        match self {
            Self::Edge { monitor_id, .. } => monitor_id,
            Self::Corner { monitor_id, .. } => monitor_id,
        }
    }

    pub fn edge(&self) -> ScreenEdge {
        match self {
            Self::Edge { edge, .. } => *edge,
            Self::Corner { zone, .. } => zone.edge(),
        }
    }
}

/// Drag-to-edge capture state machine driven strictly during `MOVESIZESTART`..`MOVESIZEEND`
/// (zero `WH_MOUSE_LL` hooks, zero idle CPU).
#[derive(Debug, Clone)]
pub struct DragCaptureStateMachine {
    pub edge_zone_px: i32,
    pub corner_span_px: i32,
    pub dwell_ms: u64,
    pub inset_px: i32,
    pub require_modifier: bool,
    pub enabled_edges: [bool; 4],
    dragging_hwnd: Option<WindowHandle>,
    hovering_target: Option<(DragDropTarget, u64)>,
    armed_target: Option<DragDropTarget>,
}

impl DragCaptureStateMachine {
    pub fn new(edge_zone_px: i32, dwell_ms: u64) -> Self {
        Self {
            edge_zone_px: edge_zone_px.max(2),
            corner_span_px: 96,
            dwell_ms,
            inset_px: 0,
            require_modifier: false,
            enabled_edges: [true, true, false, true],
            dragging_hwnd: None,
            hovering_target: None,
            armed_target: None,
        }
    }

    pub fn with_corner_span(mut self, corner_span_px: i32) -> Self {
        self.corner_span_px = corner_span_px.max(16);
        self
    }

    pub fn with_inset(mut self, inset_px: i32) -> Self {
        self.inset_px = inset_px.max(0);
        self
    }

    pub fn with_require_modifier(mut self, require: bool) -> Self {
        self.require_modifier = require;
        self
    }

    pub fn with_enabled_edges(mut self, enabled: [bool; 4]) -> Self {
        self.enabled_edges = enabled;
        self
    }

    pub fn is_edge_enabled(&self, edge: ScreenEdge) -> bool {
        match edge {
            ScreenEdge::Left => self.enabled_edges[0],
            ScreenEdge::Right => self.enabled_edges[1],
            ScreenEdge::Top => self.enabled_edges[2],
            ScreenEdge::Bottom => self.enabled_edges[3],
        }
    }

    pub fn is_dragging(&self) -> bool {
        self.dragging_hwnd.is_some()
    }

    pub fn hovering_target(&self) -> Option<&DragDropTarget> {
        self.hovering_target.as_ref().map(|(t, _)| t)
    }

    pub fn armed_target(&self) -> Option<&DragDropTarget> {
        self.armed_target.as_ref()
    }

    pub fn begin_drag(&mut self, hwnd: WindowHandle) {
        self.dragging_hwnd = Some(hwnd);
        self.hovering_target = None;
        self.armed_target = None;
    }

    /// Polled ONLY while a window move/size loop is active.
    /// Returns the currently armed [`DragDropTarget`] if preview should be shown.
    pub fn update_drag(
        &mut self,
        monitors: &[MonitorInfo],
        cursor: (i32, i32),
        modifier_held: bool,
        now_ms: u64,
    ) -> Option<DragDropTarget> {
        self.dragging_hwnd?;

        if self.require_modifier && !modifier_held {
            self.hovering_target = None;
            self.armed_target = None;
            return None;
        }

        let effective_inset = if modifier_held || self.require_modifier {
            self.inset_px
        } else {
            0
        };

        let detected = detect_drag_drop_zone_ext(
            monitors,
            self.edge_zone_px,
            self.corner_span_px,
            effective_inset,
            |e| self.is_edge_enabled(e),
            cursor,
        );
        match detected {
            Some(target) => {
                if modifier_held || self.dwell_ms == 0 {
                    self.hovering_target = Some((target.clone(), now_ms));
                    self.armed_target = Some(target);
                } else {
                    match &self.hovering_target {
                        Some((prev_target, entered_at)) if *prev_target == target => {
                            if now_ms.saturating_sub(*entered_at) >= self.dwell_ms {
                                self.armed_target = Some(target);
                            }
                        }
                        _ => {
                            self.hovering_target = Some((target.clone(), now_ms));
                            self.armed_target = None;
                        }
                    }
                }
            }
            None => {
                self.hovering_target = None;
                self.armed_target = None;
            }
        }

        self.armed_target.clone()
    }

    /// Finalizes drag on `EVENT_SYSTEM_MOVESIZEEND`, returning capture target if armed.
    pub fn end_drag(&mut self) -> Option<(WindowHandle, DragDropTarget)> {
        let hwnd = self.dragging_hwnd.take()?;
        self.hovering_target = None;
        let target = self.armed_target.take()?;
        Some((hwnd, target))
    }
}

/// Extended detection with configurable inset and per-edge enabling.
pub fn detect_drag_drop_zone_ext(
    monitors: &[MonitorInfo],
    edge_zone_px: i32,
    corner_span_px: i32,
    inset_px: i32,
    is_edge_enabled: impl Fn(ScreenEdge) -> bool,
    cursor: (i32, i32),
) -> Option<DragDropTarget> {
    let (x, y) = cursor;
    let ins = inset_px.max(0);
    for mon in monitors {
        let r = mon.rect;
        if !r.contains_inclusive(x, y) {
            continue;
        }
        let c_span = corner_span_px.max(32);
        let e_zone = edge_zone_px.max(4);

        // 1. Right edge corners (TopRightDown, BottomRightUp)
        if is_edge_enabled(ScreenEdge::Right) && x >= r.right - c_span - ins && x <= r.right - ins {
            if y >= r.top + ins && y <= r.top + c_span + ins {
                return Some(DragDropTarget::Corner {
                    monitor_id: mon.id.clone(),
                    zone: ChurnZoneId::TopRightDown,
                });
            }
            if y >= r.bottom - c_span - ins && y <= r.bottom - ins {
                return Some(DragDropTarget::Corner {
                    monitor_id: mon.id.clone(),
                    zone: ChurnZoneId::BottomRightUp,
                });
            }
        }

        // 2. Left edge corners (TopLeftDown, BottomLeftUp)
        if is_edge_enabled(ScreenEdge::Left) && x >= r.left + ins && x <= r.left + c_span + ins {
            if y >= r.top + ins && y <= r.top + c_span + ins {
                return Some(DragDropTarget::Corner {
                    monitor_id: mon.id.clone(),
                    zone: ChurnZoneId::TopLeftDown,
                });
            }
            if y >= r.bottom - c_span - ins && y <= r.bottom - ins {
                return Some(DragDropTarget::Corner {
                    monitor_id: mon.id.clone(),
                    zone: ChurnZoneId::BottomLeftUp,
                });
            }
        }

        // 3. Top edge corners (TopLeftRight, TopRightLeft)
        if is_edge_enabled(ScreenEdge::Top) && y >= r.top + ins && y <= r.top + c_span + ins {
            if x >= r.left + ins && x <= r.left + c_span + ins {
                return Some(DragDropTarget::Corner {
                    monitor_id: mon.id.clone(),
                    zone: ChurnZoneId::TopLeftRight,
                });
            }
            if x >= r.right - c_span - ins && x <= r.right - ins {
                return Some(DragDropTarget::Corner {
                    monitor_id: mon.id.clone(),
                    zone: ChurnZoneId::TopRightLeft,
                });
            }
        }

        // 4. Bottom edge corners (BottomLeftRight, BottomRightLeft)
        if is_edge_enabled(ScreenEdge::Bottom)
            && y >= r.bottom - c_span - ins
            && y <= r.bottom - ins
        {
            if x >= r.left + ins && x <= r.left + c_span + ins {
                return Some(DragDropTarget::Corner {
                    monitor_id: mon.id.clone(),
                    zone: ChurnZoneId::BottomLeftRight,
                });
            }
            if x >= r.right - c_span - ins && x <= r.right - ins {
                return Some(DragDropTarget::Corner {
                    monitor_id: mon.id.clone(),
                    zone: ChurnZoneId::BottomRightLeft,
                });
            }
        }

        // 5. Center edge drop zones (bounded strictly between the two corners)
        if is_edge_enabled(ScreenEdge::Left)
            && x >= r.left + ins
            && x <= r.left + e_zone + ins
            && y > r.top + c_span + ins
            && y < r.bottom - c_span - ins
        {
            return Some(DragDropTarget::Edge {
                monitor_id: mon.id.clone(),
                edge: ScreenEdge::Left,
            });
        }
        if is_edge_enabled(ScreenEdge::Right)
            && x >= r.right - e_zone - ins
            && x <= r.right - ins
            && y > r.top + c_span + ins
            && y < r.bottom - c_span - ins
        {
            return Some(DragDropTarget::Edge {
                monitor_id: mon.id.clone(),
                edge: ScreenEdge::Right,
            });
        }
        if is_edge_enabled(ScreenEdge::Top)
            && y >= r.top + ins
            && y <= r.top + e_zone + ins
            && x > r.left + c_span + ins
            && x < r.right - c_span - ins
        {
            return Some(DragDropTarget::Edge {
                monitor_id: mon.id.clone(),
                edge: ScreenEdge::Top,
            });
        }
        if is_edge_enabled(ScreenEdge::Bottom)
            && y >= r.bottom - e_zone - ins
            && y <= r.bottom - ins
            && x > r.left + c_span + ins
            && x < r.right - c_span - ins
        {
            return Some(DragDropTarget::Edge {
                monitor_id: mon.id.clone(),
                edge: ScreenEdge::Bottom,
            });
        }
    }
    None
}

/// Detects whether cursor lies within dedicated corner drop zones (~96px) or center edge drop zones.
pub fn detect_drag_drop_zone(
    monitors: &[MonitorInfo],
    edge_zone_px: i32,
    corner_span_px: i32,
    cursor: (i32, i32),
) -> Option<DragDropTarget> {
    detect_drag_drop_zone_ext(monitors, edge_zone_px, corner_span_px, 0, |_| true, cursor)
}

/// Computes the overlay window boundary spanning all 3 drop zones along `edge` with inset offset.
pub fn compute_edge_band_rect_inset(
    mon: &MonitorInfo,
    edge: ScreenEdge,
    edge_zone_px: i32,
    corner_span_px: i32,
    inset_px: i32,
) -> Rect {
    let r = mon.rect;
    let band_depth = edge_zone_px.max(corner_span_px);
    let ins = inset_px.max(0);
    match edge {
        ScreenEdge::Left => Rect::new(
            r.left + ins,
            r.top + ins,
            r.left + band_depth + ins,
            r.bottom - ins,
        ),
        ScreenEdge::Right => Rect::new(
            r.right - band_depth - ins,
            r.top + ins,
            r.right - ins,
            r.bottom - ins,
        ),
        ScreenEdge::Top => Rect::new(
            r.left + ins,
            r.top + ins,
            r.right - ins,
            r.top + band_depth + ins,
        ),
        ScreenEdge::Bottom => Rect::new(
            r.left + ins,
            r.bottom - band_depth - ins,
            r.right - ins,
            r.bottom - ins,
        ),
    }
}

/// Computes the overlay window boundary spanning all 3 drop zones along `edge`.
pub fn compute_edge_band_rect(
    mon: &MonitorInfo,
    edge: ScreenEdge,
    edge_zone_px: i32,
    corner_span_px: i32,
) -> Rect {
    compute_edge_band_rect_inset(mon, edge, edge_zone_px, corner_span_px, 0)
}

/// Detects if cursor is near any enabled monitor perimeter or inset band, returning the nearest edge.
pub fn detect_near_edge_ext(
    monitors: &[MonitorInfo],
    proximity_px: i32,
    inset_px: i32,
    is_edge_enabled: impl Fn(ScreenEdge) -> bool,
    cursor: (i32, i32),
) -> Option<(&MonitorInfo, ScreenEdge)> {
    let (x, y) = cursor;
    let ins = inset_px.max(0);
    for mon in monitors {
        let r = mon.rect;
        if !r.contains_inclusive(x, y) {
            continue;
        }

        let mut best: Option<(i32, ScreenEdge)> = None;
        let mut check_edge = |dist: i32, edge: ScreenEdge| {
            if dist <= proximity_px {
                match best {
                    Some((b_dist, _)) if dist < b_dist => best = Some((dist, edge)),
                    None => best = Some((dist, edge)),
                    _ => {}
                }
            }
        };

        if is_edge_enabled(ScreenEdge::Left) {
            let target_x = r.left + ins;
            let dist_left = (x - target_x).abs();
            check_edge(dist_left, ScreenEdge::Left);
        }
        if is_edge_enabled(ScreenEdge::Right) {
            let target_x = r.right - ins;
            let dist_right = (x - target_x).abs();
            check_edge(dist_right, ScreenEdge::Right);
        }
        if is_edge_enabled(ScreenEdge::Top) {
            let target_y = r.top + ins;
            let dist_top = (y - target_y).abs();
            check_edge(dist_top, ScreenEdge::Top);
        }
        if is_edge_enabled(ScreenEdge::Bottom) {
            let target_y = r.bottom - ins;
            let dist_bottom = (y - target_y).abs();
            check_edge(dist_bottom, ScreenEdge::Bottom);
        }

        if let Some((_, edge)) = best {
            return Some((mon, edge));
        }
    }
    None
}

/// Detects if cursor is near any monitor perimeter, returning the nearest edge.
pub fn detect_near_edge(
    monitors: &[MonitorInfo],
    proximity_px: i32,
    cursor: (i32, i32),
) -> Option<(&MonitorInfo, ScreenEdge)> {
    detect_near_edge_ext(monitors, proximity_px, 0, |_| true, cursor)
}

/// Computes the visual rectangular highlight for a drop target on a monitor.
pub fn compute_drop_preview_rect(mon: &MonitorInfo, target: &DragDropTarget) -> Rect {
    let r = mon.rect;
    match target {
        DragDropTarget::Edge { edge, .. } => trigger_strip_rect(mon, *edge, 36),
        DragDropTarget::Corner { zone, .. } => {
            let size = 96;
            match zone {
                ChurnZoneId::TopLeftDown | ChurnZoneId::TopLeftRight => {
                    Rect::new(r.left, r.top, r.left + size, r.top + size)
                }
                ChurnZoneId::TopRightDown | ChurnZoneId::TopRightLeft => {
                    Rect::new(r.right - size, r.top, r.right, r.top + size)
                }
                ChurnZoneId::BottomLeftUp | ChurnZoneId::BottomLeftRight => {
                    Rect::new(r.left, r.bottom - size, r.left + size, r.bottom)
                }
                ChurnZoneId::BottomRightUp | ChurnZoneId::BottomRightLeft => {
                    Rect::new(r.right - size, r.bottom - size, r.right, r.bottom)
                }
            }
        }
    }
}

/// Decision returned when a mouse wheel tick occurs on the thin edge trigger strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripScrollDecision {
    PassToWindow,
    CycleWindow,
    CycleAndOpenIfClosed,
}

/// Per-edge state machine governing Area-Aware Scroll Behavior along the thin trigger strip.
#[derive(Debug, Default, Clone)]
pub struct AreaAwareScrollController {
    entered_strip_while_armed: bool,
    opened_via_edge: bool,
    disarmed_for_open_session: bool,
}

impl AreaAwareScrollController {
    /// Marks cursor entering the thin trigger line.
    pub fn on_strip_enter(&mut self, is_open: bool) {
        if !is_open {
            self.disarmed_for_open_session = false;
            self.entered_strip_while_armed = true;
        } else if !self.disarmed_for_open_session {
            self.entered_strip_while_armed = true;
        }
    }

    /// Marks cursor leaving the thin trigger line (if `into_drawer_body`, disarms initial-session scroll).
    pub fn on_strip_leave(&mut self, into_drawer_body: bool) {
        if into_drawer_body && self.entered_strip_while_armed {
            self.disarmed_for_open_session = true;
        }
        self.entered_strip_while_armed = false;
    }

    /// Updates state when the drawer on this edge finishes opening.
    pub fn on_drawer_opened(&mut self, opened_via_edge: bool, cursor_on_strip: bool) {
        self.opened_via_edge = opened_via_edge;
        if cursor_on_strip && (opened_via_edge || !self.disarmed_for_open_session) {
            self.entered_strip_while_armed = true;
            self.disarmed_for_open_session = false;
        } else if !opened_via_edge {
            self.entered_strip_while_armed = false;
            self.disarmed_for_open_session = true;
        }
    }

    /// Resets session flags when the drawer closes so the next edge contact starts fresh.
    pub fn on_drawer_closed(&mut self) {
        self.opened_via_edge = false;
        self.disarmed_for_open_session = false;
    }

    /// Returns whether the thin trigger line should intercept mouse wheel scrolling on this edge.
    pub fn should_strip_capture_pointer(
        &self,
        enabled: bool,
        mode: AreaAwareScrollMode,
        scroll_trigger_line_only_when_open: bool,
        is_open: bool,
        has_slots: bool,
    ) -> bool {
        if !enabled || !has_slots {
            return false;
        }
        if scroll_trigger_line_only_when_open && !is_open {
            return false;
        }
        match mode {
            AreaAwareScrollMode::InitialTriggerSession => {
                !is_open || !self.disarmed_for_open_session
            }
            AreaAwareScrollMode::AlwaysOnTriggerLine => true,
            AreaAwareScrollMode::OpenSessionStrict => {
                is_open && self.opened_via_edge && !self.disarmed_for_open_session
            }
        }
    }

    /// Returns whether the 4px glowing hairline bezel shade should be visible right now.
    pub fn is_shade_visible(
        &self,
        enabled: bool,
        mode: AreaAwareScrollMode,
        scroll_trigger_line_only_when_open: bool,
        is_open: bool,
        has_slots: bool,
        cursor_on_line: bool,
    ) -> bool {
        cursor_on_line
            && self.should_strip_capture_pointer(
                enabled,
                mode,
                scroll_trigger_line_only_when_open,
                is_open,
                has_slots,
            )
    }

    /// Evaluates what action a wheel scroll on the thin trigger strip should trigger.
    pub fn on_strip_wheel(
        &self,
        enabled: bool,
        mode: AreaAwareScrollMode,
        scroll_trigger_line_only_when_open: bool,
        is_open: bool,
        cursor_on_strip: bool,
        has_slots: bool,
    ) -> StripScrollDecision {
        if !enabled || !has_slots || !cursor_on_strip {
            return StripScrollDecision::PassToWindow;
        }
        if scroll_trigger_line_only_when_open && !is_open {
            return StripScrollDecision::PassToWindow;
        }
        match mode {
            AreaAwareScrollMode::InitialTriggerSession => {
                if is_open {
                    if !self.disarmed_for_open_session {
                        StripScrollDecision::CycleWindow
                    } else {
                        StripScrollDecision::PassToWindow
                    }
                } else {
                    StripScrollDecision::CycleAndOpenIfClosed
                }
            }
            AreaAwareScrollMode::AlwaysOnTriggerLine => {
                if is_open {
                    StripScrollDecision::CycleWindow
                } else {
                    StripScrollDecision::CycleAndOpenIfClosed
                }
            }
            AreaAwareScrollMode::OpenSessionStrict => {
                if is_open && self.opened_via_edge && !self.disarmed_for_open_session {
                    StripScrollDecision::CycleWindow
                } else {
                    StripScrollDecision::PassToWindow
                }
            }
        }
    }
}

/// Computes the 4px glowing bezel bar rectangle along `monitor.work_rect` for Area-Aware Scroll mode.
pub fn area_scroll_shade_rect(mon: &MonitorInfo, edge: ScreenEdge) -> Rect {
    let w = mon.work_rect;
    match edge {
        ScreenEdge::Left => Rect::new(w.left, w.top, (w.left + 4).min(w.right), w.bottom),
        ScreenEdge::Right => Rect::new((w.right - 4).max(w.left), w.top, w.right, w.bottom),
        ScreenEdge::Top => Rect::new(w.left, w.top, w.right, (w.top + 4).min(w.bottom)),
        ScreenEdge::Bottom => Rect::new(w.left, (w.bottom - 4).max(w.top), w.right, w.bottom),
    }
}

/// Returns `true` if `cursor` lies along the 4px area-aware edge scroll band for `(mon, edge)`.
pub fn is_in_area_scroll_line(mon: &MonitorInfo, edge: ScreenEdge, cursor: (i32, i32)) -> bool {
    is_in_bezel_corridor(mon.rect, edge, 4, cursor)
        || area_scroll_shade_rect(mon, edge).contains_inclusive(cursor.0, cursor.1)
}

/// Returns whether an auxiliary strip window should be click/scroll-transparent.
/// Ensures EdgeBump & CornerSlide & closed OpenSessionStrict never block right-edge scrollbars.
pub fn is_strip_input_transparent(
    gesture_mode: GestureMode,
    area_scroll_enabled: bool,
    scroll_mode: AreaAwareScrollMode,
    is_open: bool,
    has_slots: bool,
) -> bool {
    if matches!(
        gesture_mode,
        GestureMode::EdgeBump | GestureMode::CornerSlide
    ) && !is_open
    {
        return true;
    }
    if !area_scroll_enabled || !has_slots {
        return gesture_mode != GestureMode::EdgeHover || is_open;
    }
    match scroll_mode {
        AreaAwareScrollMode::OpenSessionStrict => !is_open,
        AreaAwareScrollMode::InitialTriggerSession | AreaAwareScrollMode::AlwaysOnTriggerLine => {
            matches!(
                gesture_mode,
                GestureMode::EdgeBump | GestureMode::CornerSlide
            ) && !is_open
        }
    }
}

/// Computes whether an edge indicator pill should currently be visible, honoring `auto_hide_pill`
/// and revealing automatically when a stashed window requests attention.
pub fn should_pill_be_visible(
    style: EdgePillStyle,
    auto_hide_pill: bool,
    slot_count: usize,
    is_open: bool,
    is_animating: bool,
    has_attention: bool,
) -> bool {
    if is_animating {
        return true;
    }
    if style == EdgePillStyle::Hidden || slot_count == 0 {
        return false;
    }
    if has_attention {
        return true;
    }
    !auto_hide_pill || is_open
}

/// Returns the bottommost open floating drawer HWND to pass as `hWndInsertAfter` for `dim_scrim`
/// so the backdrop scrim is atomically ordered beneath open drawers without z-order flashing.
pub fn compute_dim_scrim_insert_after(
    open_floating_hwnds: &[WindowHandle],
) -> Option<WindowHandle> {
    open_floating_hwnds.last().copied()
}
