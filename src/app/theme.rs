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

#[cfg(windows)]
mod user32 {
    unsafe extern "system" {
        pub fn SetWindowPos(
            hwnd: isize,
            hwnd_insert_after: isize,
            x: i32,
            y: i32,
            cx: i32,
            cy: i32,
            flags: u32,
        ) -> i32;
    }
}

#[cfg(windows)]
mod dwmapi {
    unsafe extern "system" {
        pub fn DwmSetWindowAttribute(
            hwnd: isize,
            attribute: u32,
            value: *const std::ffi::c_void,
            size: u32,
        ) -> i32;
    }
}

/// Красит системную рамку окна (заголовок) в тёмный режим напрямую через
/// официально задокументированный `DWMWA_USE_IMMERSIVE_DARK_MODE` - а не
/// через приватный API, которым для этого пользуется winit
/// (`ViewportCommand::SetTheme`), и который на практике не всегда реально
/// перекрашивает заголовок. Сразу же форсирует перерисовку рамки через
/// `SetWindowPos(.., SWP_FRAMECHANGED)`, иначе она визуально останется
/// прежней до следующего системного перерисовывания (например,
/// сворачивания/разворачивания окна).
pub fn apply_dark_titlebar(window: &impl raw_window_handle::HasWindowHandle) {
    #[cfg(windows)]
    {
        use raw_window_handle::RawWindowHandle;

        const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
        const SWP_NOSIZE: u32 = 0x0001;
        const SWP_NOMOVE: u32 = 0x0002;
        const SWP_NOZORDER: u32 = 0x0004;
        const SWP_NOACTIVATE: u32 = 0x0010;
        const SWP_FRAMECHANGED: u32 = 0x0020;

        if let Ok(handle) = window.window_handle()
            && let RawWindowHandle::Win32(win32) = handle.as_raw()
        {
            let hwnd = win32.hwnd.get();
            let enabled: i32 = 1;
            unsafe {
                dwmapi::DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_USE_IMMERSIVE_DARK_MODE,
                    (&raw const enabled).cast(),
                    size_of::<i32>() as u32,
                );
                user32::SetWindowPos(
                    hwnd,
                    0,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                );
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = window;
    }
}
