// состояние диалога захвата горячей клавиши + преобразование клавиш egui -> global_hotkey

use eframe::egui;
use global_hotkey::hotkey::{Code as HkCode, HotKey, Modifiers as HkModifiers};

pub struct HotkeyCapture {
    pub captured: Option<HotKey>,
}

impl HotkeyCapture {
    pub fn new() -> Self {
        Self { captured: None }
    }
}

/// Преобразует клавишу egui в физический код клавиши `global_hotkey`.
/// Покрывает буквы, цифры, функциональные клавиши и основные клавиши
/// навигации/редактирования
pub fn egui_key_to_code(key: egui::Key) -> Option<HkCode> {
    use egui::Key;
    Some(match key {
        Key::A => HkCode::KeyA,
        Key::B => HkCode::KeyB,
        Key::C => HkCode::KeyC,
        Key::D => HkCode::KeyD,
        Key::E => HkCode::KeyE,
        Key::F => HkCode::KeyF,
        Key::G => HkCode::KeyG,
        Key::H => HkCode::KeyH,
        Key::I => HkCode::KeyI,
        Key::J => HkCode::KeyJ,
        Key::K => HkCode::KeyK,
        Key::L => HkCode::KeyL,
        Key::M => HkCode::KeyM,
        Key::N => HkCode::KeyN,
        Key::O => HkCode::KeyO,
        Key::P => HkCode::KeyP,
        Key::Q => HkCode::KeyQ,
        Key::R => HkCode::KeyR,
        Key::S => HkCode::KeyS,
        Key::T => HkCode::KeyT,
        Key::U => HkCode::KeyU,
        Key::V => HkCode::KeyV,
        Key::W => HkCode::KeyW,
        Key::X => HkCode::KeyX,
        Key::Y => HkCode::KeyY,
        Key::Z => HkCode::KeyZ,
        Key::Num0 => HkCode::Digit0,
        Key::Num1 => HkCode::Digit1,
        Key::Num2 => HkCode::Digit2,
        Key::Num3 => HkCode::Digit3,
        Key::Num4 => HkCode::Digit4,
        Key::Num5 => HkCode::Digit5,
        Key::Num6 => HkCode::Digit6,
        Key::Num7 => HkCode::Digit7,
        Key::Num8 => HkCode::Digit8,
        Key::Num9 => HkCode::Digit9,
        Key::F1 => HkCode::F1,
        Key::F2 => HkCode::F2,
        Key::F3 => HkCode::F3,
        Key::F4 => HkCode::F4,
        Key::F5 => HkCode::F5,
        Key::F6 => HkCode::F6,
        Key::F7 => HkCode::F7,
        Key::F8 => HkCode::F8,
        Key::F9 => HkCode::F9,
        Key::F10 => HkCode::F10,
        Key::F11 => HkCode::F11,
        Key::F12 => HkCode::F12,
        Key::Escape => HkCode::Escape,
        Key::Tab => HkCode::Tab,
        Key::Backspace => HkCode::Backspace,
        Key::Enter => HkCode::Enter,
        Key::Space => HkCode::Space,
        Key::Insert => HkCode::Insert,
        Key::Delete => HkCode::Delete,
        Key::Home => HkCode::Home,
        Key::End => HkCode::End,
        Key::PageUp => HkCode::PageUp,
        Key::PageDown => HkCode::PageDown,
        Key::ArrowUp => HkCode::ArrowUp,
        Key::ArrowDown => HkCode::ArrowDown,
        Key::ArrowLeft => HkCode::ArrowLeft,
        Key::ArrowRight => HkCode::ArrowRight,
        _ => return None,
    })
}

pub fn egui_modifiers_to_hk(modifiers: egui::Modifiers) -> Option<HkModifiers> {
    let mut mods = HkModifiers::empty();
    if modifiers.ctrl {
        mods |= HkModifiers::CONTROL;
    }
    if modifiers.shift {
        mods |= HkModifiers::SHIFT;
    }
    if modifiers.alt {
        mods |= HkModifiers::ALT;
    }
    if mods.is_empty() { None } else { Some(mods) }
}
