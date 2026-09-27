use eframe::egui::{self, Color32, FontId, Stroke, TextStyle};

pub(super) const SURFACE: Color32 = Color32::from_rgb(25, 34, 46);
pub(super) const SELECTED: Color32 = Color32::from_rgb(26, 51, 60);
pub(super) const BORDER: Color32 = Color32::from_rgb(49, 63, 80);
pub(super) const ACCENT: Color32 = Color32::from_rgb(113, 219, 196);
pub(super) const MUTED: Color32 = Color32::from_rgb(159, 177, 197);

/// Applies the single built-in palette and spacing to every desktop viewport.
pub(super) fn apply(context: &egui::Context) {
    context.set_theme(egui::Theme::Dark);
    let mut style = egui::Style::default();
    style.text_styles.extend([
        (TextStyle::Heading, FontId::proportional(22.0)),
        (TextStyle::Body, FontId::proportional(15.0)),
        (TextStyle::Button, FontId::proportional(14.0)),
        (TextStyle::Small, FontId::proportional(12.0)),
        (TextStyle::Monospace, FontId::monospace(13.0)),
    ]);
    style.spacing.item_spacing = egui::vec2(10.0, 9.0);
    style.spacing.button_padding = egui::vec2(12.0, 7.0);
    style.spacing.interact_size = egui::vec2(36.0, 32.0);
    style.spacing.window_margin = egui::Margin::same(18);
    style.visuals = egui::Visuals::dark();
    let visuals = &mut style.visuals;
    visuals.panel_fill = Color32::from_rgb(16, 23, 33);
    visuals.window_fill = SURFACE;
    visuals.extreme_bg_color = Color32::from_rgb(12, 19, 28);
    visuals.faint_bg_color = SURFACE;
    visuals.code_bg_color = SURFACE;
    visuals.weak_text_color = Some(MUTED);
    visuals.hyperlink_color = ACCENT;
    visuals.selection.bg_fill = SELECTED;
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    visuals.window_stroke = Stroke::new(1.0, BORDER);
    visuals.window_corner_radius = egui::CornerRadius::same(12);
    visuals.menu_corner_radius = egui::CornerRadius::same(8);
    visuals.warn_fg_color = Color32::from_rgb(245, 197, 112);
    visuals.error_fg_color = Color32::from_rgb(255, 143, 153);
    let widgets = &mut visuals.widgets;
    for widget in [
        &mut widgets.noninteractive,
        &mut widgets.inactive,
        &mut widgets.hovered,
        &mut widgets.active,
        &mut widgets.open,
    ] {
        widget.corner_radius = egui::CornerRadius::same(7);
        widget.fg_stroke = Stroke::new(1.0, Color32::from_rgb(227, 235, 245));
        widget.bg_stroke = Stroke::new(1.0, BORDER);
        widget.bg_fill = SURFACE;
        widget.weak_bg_fill = SURFACE;
    }
    for widget in [&mut widgets.hovered, &mut widgets.active, &mut widgets.open] {
        widget.bg_fill = SELECTED;
        widget.weak_bg_fill = SELECTED;
        widget.bg_stroke = Stroke::new(1.0, ACCENT);
    }
    widgets.active.bg_fill = Color32::from_rgb(39, 94, 84);
    widgets.active.weak_bg_fill = widgets.active.bg_fill;
    context.set_global_style(style);
}
