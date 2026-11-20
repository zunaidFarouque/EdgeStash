use egui::{Color32, CornerRadius, Frame, Margin, Stroke, Vec2};

/// Active visual appearance theme for `EdgeStash Settings`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeMode {
    Dark,
    Light,
}

impl ThemeMode {
    pub fn toggle(self) -> Self {
        match self {
            Self::Dark => Self::Light,
            Self::Light => Self::Dark,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::Light => "Light",
        }
    }
}

/// Cohesive Fluent-inspired color tokens.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub mode: ThemeMode,
    pub bg_app: Color32,
    pub bg_sidebar: Color32,
    pub bg_card: Color32,
    pub bg_card_alt: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub text_primary: Color32,
    pub text_secondary: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub on_accent: Color32,
    pub success: Color32,
    pub success_soft: Color32,
    pub warning: Color32,
    pub warning_soft: Color32,
    pub error: Color32,
    pub error_soft: Color32,
    pub switch_off: Color32,
}

impl Palette {
    pub fn for_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Dark => Self {
                mode,
                bg_app: Color32::from_rgb(17, 20, 27),
                bg_sidebar: Color32::from_rgb(22, 26, 35),
                bg_card: Color32::from_rgb(28, 33, 44),
                bg_card_alt: Color32::from_rgb(35, 41, 55),
                border: Color32::from_rgb(45, 53, 70),
                border_strong: Color32::from_rgb(66, 77, 100),
                text_primary: Color32::from_rgb(242, 246, 252),
                text_secondary: Color32::from_rgb(154, 165, 184),
                accent: Color32::from_rgb(76, 194, 255),
                accent_soft: Color32::from_rgba_premultiplied(24, 62, 86, 255),
                on_accent: Color32::from_rgb(10, 18, 28),
                success: Color32::from_rgb(63, 185, 80),
                success_soft: Color32::from_rgba_premultiplied(22, 58, 36, 255),
                warning: Color32::from_rgb(227, 179, 65),
                warning_soft: Color32::from_rgba_premultiplied(64, 49, 20, 255),
                error: Color32::from_rgb(248, 81, 73),
                error_soft: Color32::from_rgba_premultiplied(68, 26, 28, 255),
                switch_off: Color32::from_rgb(56, 65, 84),
            },
            ThemeMode::Light => Self {
                mode,
                bg_app: Color32::from_rgb(243, 245, 249),
                bg_sidebar: Color32::from_rgb(234, 238, 245),
                bg_card: Color32::from_rgb(255, 255, 255),
                bg_card_alt: Color32::from_rgb(246, 248, 252),
                border: Color32::from_rgb(214, 221, 232),
                border_strong: Color32::from_rgb(176, 188, 207),
                text_primary: Color32::from_rgb(24, 29, 39),
                text_secondary: Color32::from_rgb(89, 99, 119),
                accent: Color32::from_rgb(0, 103, 192),
                accent_soft: Color32::from_rgb(219, 236, 252),
                on_accent: Color32::WHITE,
                success: Color32::from_rgb(26, 127, 55),
                success_soft: Color32::from_rgb(218, 244, 225),
                warning: Color32::from_rgb(154, 103, 0),
                warning_soft: Color32::from_rgb(254, 242, 207),
                error: Color32::from_rgb(207, 34, 46),
                error_soft: Color32::from_rgb(254, 226, 228),
                switch_off: Color32::from_rgb(186, 194, 208),
            },
        }
    }
}

/// Applies the Fluent dark/light palette and spacing rules to the `egui` context.
pub fn apply_theme(ctx: &egui::Context, mode: ThemeMode) -> Palette {
    let p = Palette::for_mode(mode);
    let mut style = (*ctx.style()).clone();

    style.visuals = match mode {
        ThemeMode::Dark => egui::Visuals::dark(),
        ThemeMode::Light => egui::Visuals::light(),
    };
    style.visuals.panel_fill = p.bg_app;
    style.visuals.window_fill = p.bg_card;
    style.visuals.extreme_bg_color = p.bg_card_alt;
    style.visuals.faint_bg_color = p.bg_sidebar;
    style.visuals.override_text_color = Some(p.text_primary);
    style.visuals.selection.bg_fill = p.accent_soft;
    style.visuals.selection.stroke = Stroke::new(1.0_f32, p.accent);

    style.visuals.widgets.noninteractive.bg_fill = p.bg_card;
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.border);
    style.visuals.widgets.noninteractive.corner_radius = CornerRadius::same(8);

    style.visuals.widgets.inactive.bg_fill = p.bg_card_alt;
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, p.border);
    style.visuals.widgets.inactive.corner_radius = CornerRadius::same(7);
    style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, p.text_primary);

    style.visuals.widgets.hovered.bg_fill = p.accent_soft;
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, p.accent);
    style.visuals.widgets.hovered.corner_radius = CornerRadius::same(7);
    style.visuals.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, p.text_primary);

    style.visuals.widgets.active.bg_fill = p.accent;
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, p.accent);
    style.visuals.widgets.active.corner_radius = CornerRadius::same(7);
    style.visuals.widgets.active.fg_stroke = Stroke::new(1.0_f32, p.on_accent);

    style.spacing.item_spacing = Vec2::new(10.0, 8.0);
    style.spacing.button_padding = Vec2::new(12.0, 6.0);
    style.spacing.slider_width = 210.0;
    style.spacing.interact_size.y = 28.0;

    ctx.set_style(style);
    p
}

/// Framed Fluent settings card container.
pub fn card_frame(p: &Palette) -> Frame {
    Frame::NONE
        .fill(p.bg_card)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(14))
}

/// Renders a section card with title + optional subtitle.
pub fn card(
    ui: &mut egui::Ui,
    p: &Palette,
    title: &str,
    subtitle: Option<&str>,
    add_body: impl FnOnce(&mut egui::Ui),
) {
    card_frame(p).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.label(
            egui::RichText::new(title)
                .size(15.0)
                .strong()
                .color(p.text_primary),
        );
        if let Some(sub) = subtitle {
            ui.label(egui::RichText::new(sub).size(12.0).color(p.text_secondary));
        }
        ui.add_space(6.0);
        add_body(ui);
    });
}

/// Renders a rounded status pill badge.
pub fn badge_pill(ui: &mut egui::Ui, text: &str, bg: Color32, fg: Color32) {
    Frame::NONE
        .fill(bg)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::symmetric(9, 3))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).size(11.5).strong().color(fg));
        });
}

/// Renders a Fluent-style pill toggle switch paired with label & description.
pub fn toggle_row(
    ui: &mut egui::Ui,
    p: &Palette,
    value: &mut bool,
    title: &str,
    description: &str,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_max_width((ui.available_width() - 62.0).max(120.0));
            ui.label(egui::RichText::new(title).size(13.5).color(p.text_primary));
            ui.label(
                egui::RichText::new(description)
                    .size(11.5)
                    .color(p.text_secondary),
            );
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let desired = Vec2::new(42.0, 22.0);
            let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::click());
            if response.clicked() {
                *value = !*value;
                changed = true;
            }
            if ui.is_rect_visible(rect) {
                let how_on = ui.ctx().animate_bool_responsive(response.id, *value);
                let bg = if *value { p.accent } else { p.switch_off };
                let radius = 11.0;
                ui.painter().rect_filled(rect, radius, bg);
                if response.hovered() {
                    ui.painter().rect_stroke(
                        rect,
                        radius,
                        Stroke::new(1.5_f32, p.accent),
                        egui::StrokeKind::Outside,
                    );
                }
                let circle_x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), how_on);
                let thumb = if *value { p.on_accent } else { Color32::WHITE };
                ui.painter()
                    .circle_filled(egui::pos2(circle_x, rect.center().y), 7.5, thumb);
            }
        });
    });
    changed
}

/// Renders a primary solid accent button.
pub fn accent_button(ui: &mut egui::Ui, p: &Palette, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(egui::RichText::new(label).strong().color(p.on_accent))
            .fill(p.accent)
            .stroke(Stroke::new(1.0_f32, p.accent))
            .corner_radius(CornerRadius::same(7)),
    )
}
