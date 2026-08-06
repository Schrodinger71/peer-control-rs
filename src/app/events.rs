// фоновые события статуса + всплывающие уведомления в приложении

use eframe::egui::Color32;
use std::time::{Duration, Instant};

pub enum StatusEvent {
    Status {
        name: String,
        text: String,
        color: Color32,
    },
    Error {
        title: String,
        message: String,
    },
    HotkeyTriggered,
    Toast {
        message: String,
    },
}

pub struct Toast {
    pub message: String,
    pub created_at: Instant,
}

pub const TOAST_LIFETIME: Duration = Duration::from_secs(6);
