// цвета, иконка и общее оформление GUI

use eframe::egui::{self, Color32, CornerRadius};

pub const COLOR_GREEN: Color32 = Color32::from_rgb(0x4c, 0xaf, 0x50);
pub const COLOR_RED: Color32 = Color32::from_rgb(0xe5, 0x53, 0x53);
pub const COLOR_YELLOW: Color32 = Color32::from_rgb(0xff, 0xb7, 0x4d);
pub const COLOR_GRAY: Color32 = Color32::from_rgb(0x9e, 0x9e, 0x9e);
pub const COLOR_ACCENT: Color32 = Color32::from_rgb(0x5b, 0x8d, 0xef);
pub const COLOR_DANGER: Color32 = Color32::from_rgb(0xe0, 0x57, 0x57);

const ICON_PNG: &[u8] = include_bytes!("../../assets/icon.png");

pub fn decode_icon() -> (Vec<u8>, u32, u32) {
    let image = image::load_from_memory(ICON_PNG)
        .expect("decode embedded icon")
        .into_rgba8();
    let (width, height) = image.dimensions();
    (image.into_raw(), width, height)
}

/// Более спокойная и скруглённая тема, чем стандартная тёмная тема egui.
pub fn build_visuals() -> egui::Visuals {
    let mut visuals = egui::Visuals::dark();

    visuals.panel_fill = Color32::from_rgb(0x1e, 0x21, 0x28);
    visuals.window_fill = Color32::from_rgb(0x24, 0x28, 0x30);
    visuals.extreme_bg_color = Color32::from_rgb(0x17, 0x19, 0x1f);
    visuals.faint_bg_color = Color32::from_rgb(0x2a, 0x2e, 0x37);
    visuals.hyperlink_color = COLOR_ACCENT;
    visuals.selection.bg_fill = COLOR_ACCENT.linear_multiply(0.55);
    visuals.window_corner_radius = CornerRadius::same(12);
    visuals.menu_corner_radius = CornerRadius::same(8);

    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = CornerRadius::same(8);
    }
    visuals.widgets.hovered.bg_fill = COLOR_ACCENT.linear_multiply(0.35);
    visuals.widgets.active.bg_fill = COLOR_ACCENT.linear_multiply(0.55);

    visuals
}

/// Небольшая скруглённая плашка с цветной точкой и текстом статуса, например "● в сети".
pub fn status_badge(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(color.linear_multiply(0.16))
        .corner_radius(CornerRadius::same(20))
        .inner_margin(egui::Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                ui.colored_label(color, "●");
                ui.colored_label(color, text);
            });
        });
}
