#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use std::process;
use std::sync::{Arc, RwLock};

mod app;
mod config;
mod firewall;
mod logging;
mod network;

use app::PeerApp;
use config::{default_peers, load_config, load_json, log_path, peers_path, save_config};
use logging::init_logging;

// проверка прав администратора (нужна для правил файрвола / адаптеров / taskkill)
#[cfg(windows)]
mod shell32 {
    unsafe extern "system" {
        pub fn IsUserAnAdmin() -> i32;
    }
}

fn is_admin() -> bool {
    #[cfg(windows)]
    {
        unsafe { shell32::IsUserAnAdmin() != 0 }
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn main() {
    init_logging(&log_path());

    if !is_admin() {
        let message = "This peer must be run as Administrator (Windows Firewall rules require it).";
        eprintln!("{message}");
        log_error!("{message}");
        process::exit(1);
    }

    let mut initial_cfg = load_config();
    if initial_cfg.network_adapters.is_empty() {
        let adapters = firewall::discover_adapters();
        if adapters.is_empty() {
            log_warn!(
                "could not auto-detect any Wi-Fi/Ethernet adapters at startup; will retry on the next reboot_internet"
            );
        } else {
            log_info!("auto-detected adapters at startup: {adapters:?}, saving to config.json");
            initial_cfg.network_adapters = adapters;
            save_config(&initial_cfg);
        }
    }
    let cfg = Arc::new(RwLock::new(initial_cfg));
    let peers = load_json(&peers_path(), default_peers());

    let (rgba, width, height) = app::theme::decode_icon();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("peer-control-rs")
            .with_inner_size([720.0, 900.0])
            .with_min_inner_size([480.0, 420.0])
            .with_icon(egui::IconData {
                rgba,
                width,
                height,
            }),
        ..Default::default()
    };
    let run_result = eframe::run_native(
        "peer-control-rs",
        options,
        Box::new(move |cc| Ok(Box::new(PeerApp::new(cc, cfg, peers)))),
    );
    if let Err(exc) = run_result {
        log_error!("eframe run_native failed: {exc}");
    }
}
