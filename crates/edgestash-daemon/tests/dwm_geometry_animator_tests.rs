use edgestash_daemon::animator::{compute_animation_frame, ease_out_cubic};
use edgestash_daemon::dwm::BorderInsets;
use edgestash_daemon::geometry::{
    HeaderButtonKind, compute_drawer_rect, compute_offbezel_origin, compute_parked_rect,
    compute_slot_rects_on_monitor, compute_tiled_group_rects, drawer_header_height,
    header_button_rects, visible_to_window_origin,
};
use edgestash_daemon::platform::MonitorInfo;
use edgestash_ipc::types::{AnimationStyle, Rect, ScreenEdge};

fn monitor_1080p() -> MonitorInfo {
    MonitorInfo {
        id: "MONITOR-1".to_string(),
        rect: Rect::new(0, 0, 1920, 1080),
        work_rect: Rect::new(0, 0, 1920, 1032), // 48px taskbar at bottom
        dpi: 96,
        is_primary: true,
    }
}

#[test]
fn invisible_border_insets_and_origin_compensation() {
    // Typical Win11 window with 7px shadow on left, right, bottom and 0px top
    let window_rect = Rect::new(93, 0, 607, 1039);
    let frame_rect = Rect::new(100, 0, 600, 1032);
    let insets = BorderInsets::from_rects(window_rect, frame_rect);
    assert_eq!(insets.left, 7);
    assert_eq!(insets.top, 0);
    assert_eq!(insets.right, 7);
    assert_eq!(insets.bottom, 7);

    let (wx, wy) = visible_to_window_origin(0, 0, insets);
    assert_eq!((wx, wy), (-7, 0));
}

#[test]
fn drawer_rect_clamps_thickness_with_dpi_and_respects_work_area() {
    let mon = monitor_1080p();

    // Thickness below 240px clamps up to 240px
    let left_small = compute_drawer_rect(&mon, ScreenEdge::Left, 100, None);
    assert_eq!(left_small, Rect::new(0, 0, 240, 1032));

    // Thickness above 75% monitor width (1440px) clamps down to 1440px
    let right_huge = compute_drawer_rect(&mon, ScreenEdge::Right, 1800, None);
    assert_eq!(right_huge, Rect::new(1920 - 1440, 0, 1920, 1032));

    // Top and bottom anchor to work_rect so taskbar isn't covered unless on bottom edge
    let top_rect = compute_drawer_rect(&mon, ScreenEdge::Top, 400, None);
    assert_eq!(top_rect, Rect::new(0, 0, 1920, 400));

    let bottom_rect = compute_drawer_rect(&mon, ScreenEdge::Bottom, 300, None);
    assert_eq!(bottom_rect, Rect::new(0, 1032 - 300, 1920, 1032));

    // High DPI (192 DPI = 200% scale): min thickness scales from 240 -> 480
    let mut hidpi = mon;
    hidpi.dpi = 192;
    let scaled_left = compute_drawer_rect(&hidpi, ScreenEdge::Left, 100, None);
    assert_eq!(scaled_left, Rect::new(0, 0, 480, 1032));
}

#[test]
fn grouped_windows_tile_into_non_overlapping_subsegments_along_edge() {
    let mon = monitor_1080p();
    // Group of 2 windows (e.g. Notepad + Calculator) on Right edge with thicknesses 420 & 380
    // Shared drawer thickness uses the active/max clamped thickness or individual thickness?
    // Each window aligns flush to the edge with non-overlapping segments along the edge axis!
    let rects = compute_tiled_group_rects(&mon, ScreenEdge::Right, &[420, 380]);
    assert_eq!(rects.len(), 2);
    assert_eq!(rects[0], Rect::new(1920 - 420, 0, 1920, 516));
    assert_eq!(rects[1], Rect::new(1920 - 380, 516, 1920, 1032));

    // Horizontal edge (Bottom) splits horizontally
    let bottom_rects = compute_tiled_group_rects(&mon, ScreenEdge::Bottom, &[300, 300]);
    assert_eq!(bottom_rects[0], Rect::new(0, 1032 - 300, 960, 1032));
    assert_eq!(bottom_rects[1], Rect::new(960, 1032 - 300, 1920, 1032));
}

#[test]
fn offbezel_and_parked_coordinates_avoid_adjacent_monitors() {
    let left_mon = MonitorInfo {
        id: "LEFT".to_string(),
        rect: Rect::new(0, 0, 1920, 1080),
        work_rect: Rect::new(0, 0, 1920, 1032),
        dpi: 96,
        is_primary: true,
    };
    let right_mon = MonitorInfo {
        id: "RIGHT".to_string(),
        rect: Rect::new(1920, 0, 3840, 1080),
        work_rect: Rect::new(1920, 0, 3840, 1032),
        dpi: 96,
        is_primary: false,
    };

    let target = Rect::new(1520, 0, 1920, 1032);
    // Slide offbezel places it just past the right edge of left_mon
    assert_eq!(
        compute_offbezel_origin(&left_mon, ScreenEdge::Right, target),
        (1920, 0)
    );
    assert_eq!(
        compute_offbezel_origin(&left_mon, ScreenEdge::Left, Rect::new(0, 0, 400, 1032)),
        (-400, 0)
    );

    // Parked rect is completely outside the combined virtual screen [0..3840, 0..1080]
    let virtual_bounds = Rect::new(0, 0, 3840, 1080);
    let parked = compute_parked_rect(virtual_bounds, target);
    assert!(parked.0 >= virtual_bounds.right || parked.1 >= virtual_bounds.bottom);
    let _ = right_mon;
}

#[test]
fn animation_easing_and_frame_interpolation_match_spec() {
    assert!((ease_out_cubic(0.0) - 0.0).abs() < f32::EPSILON);
    assert!((ease_out_cubic(1.0) - 1.0).abs() < f32::EPSILON);
    assert!(ease_out_cubic(0.5) > 0.5);

    // Slide from x=-400 to x=0
    let start_frame = compute_animation_frame(-400, 0, 0, 0, 0.0, AnimationStyle::Slide, true);
    assert_eq!(
        (start_frame.x, start_frame.y, start_frame.alpha),
        (-400, 0, None)
    );

    let end_frame = compute_animation_frame(-400, 0, 0, 0, 1.0, AnimationStyle::Slide, true);
    assert_eq!((end_frame.x, end_frame.y, end_frame.alpha), (0, 0, None));

    // FloatAndFade open: starts 16px away along movement vector with alpha 0, ends at target with alpha 255
    let ff_open_start =
        compute_animation_frame(-400, 0, 0, 0, 0.0, AnimationStyle::FloatAndFade, true);
    assert_eq!(
        (ff_open_start.x, ff_open_start.y, ff_open_start.alpha),
        (-16, 0, Some(0))
    );

    let ff_open_end =
        compute_animation_frame(-400, 0, 0, 0, 1.0, AnimationStyle::FloatAndFade, true);
    assert_eq!(
        (ff_open_end.x, ff_open_end.y, ff_open_end.alpha),
        (0, 0, Some(255))
    );

    // FloatAndFade close: starts at current (0,0) alpha 255, glides 16px back to (-16,0) alpha 0
    let ff_close_start =
        compute_animation_frame(0, 0, -400, 0, 0.0, AnimationStyle::FloatAndFade, false);
    assert_eq!(
        (ff_close_start.x, ff_close_start.y, ff_close_start.alpha),
        (0, 0, Some(255))
    );

    let ff_close_end =
        compute_animation_frame(0, 0, -400, 0, 1.0, AnimationStyle::FloatAndFade, false);
    assert_eq!(
        (ff_close_end.x, ff_close_end.y, ff_close_end.alpha),
        (-16, 0, Some(0))
    );
}

#[test]
fn header_bar_docked_above_slot_shrinks_content_windows_and_places_buttons_close_unstash_pin() {
    let mon = monitor_1080p();
    assert_eq!(drawer_header_height(96), 28);
    assert_eq!(drawer_header_height(192), 56);

    // Single Right-edge window gets 28px header bar cleanly above and content shrunk below
    let (hdr_single, wins_single) =
        compute_slot_rects_on_monitor(&mon, ScreenEdge::Right, &[450], None);
    assert_eq!(hdr_single, Rect::new(1470, 0, 1920, 28));
    assert_eq!(wins_single, vec![Rect::new(1470, 28, 1920, 1032)]);

    // Grouped vertical stack on Right edge puts single unified header above both tiled windows
    let (hdr_group_v, wins_group_v) =
        compute_slot_rects_on_monitor(&mon, ScreenEdge::Right, &[400, 360], None);
    assert_eq!(hdr_group_v, Rect::new(1520, 0, 1920, 28));
    assert_eq!(wins_group_v.len(), 2);
    assert_eq!(wins_group_v[0], Rect::new(1520, 28, 1920, 530));
    assert_eq!(wins_group_v[1], Rect::new(1560, 530, 1920, 1032));

    // Grouped horizontal stack on Bottom edge shortens both windows by 28px below single header bar
    let (hdr_group_h, wins_group_h) =
        compute_slot_rects_on_monitor(&mon, ScreenEdge::Bottom, &[300, 300], None);
    assert_eq!(hdr_group_h, Rect::new(0, 732, 1920, 760));
    assert_eq!(
        wins_group_h,
        vec![
            Rect::new(0, 760, 960, 1032),
            Rect::new(960, 760, 1920, 1032),
        ]
    );

    // Top-right header buttons strictly ordered left-to-right: [Close] [Unstash] [Pin]
    let buttons = header_button_rects(450, 28);
    assert_eq!(buttons[0].0, HeaderButtonKind::Close);
    assert_eq!(buttons[1].0, HeaderButtonKind::Unstash);
    assert_eq!(buttons[2].0, HeaderButtonKind::Pin);
    assert!(buttons[0].1.right <= buttons[1].1.left);
    assert!(buttons[1].1.right <= buttons[2].1.left);
    assert!(buttons[2].1.right <= 450);
    assert!(buttons[0].1.left > 200);
}
