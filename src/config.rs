// конфиг / узлы / сохранение состояния

use crate::{log_error, log_warn};
use global_hotkey::hotkey::HotKey;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Адрес/порт, на котором слушает собственный сервер этого узла.
    pub host: String,
    pub port: u16,
    /// Общий секрет: одинаковый у всех узлов сети (используется и при приёме
    /// запросов, и при их отправке другим).
    pub token: String,
    /// Название(я) сетевого адаптера, который дёргается при "reboot_internet".
    /// Оставьте пустым (по умолчанию), чтобы автоматически определять все
    /// физические Wi-Fi/Ethernet адаптеры - результат записывается сюда же,
    /// чтобы он был виден и стабилен.
    #[serde(default)]
    pub network_adapters: Vec<String>,
    #[serde(default = "default_reboot_seconds")]
    pub default_reboot_seconds: u64,
    /// Глобальная горячая клавиша, которая мгновенно запускает "перезагрузить
    /// интернет у всех" из GUI этого узла. Работает даже без фокуса окна.
    #[serde(default)]
    pub hotkey: Option<HotKey>,
}

fn default_reboot_seconds() -> u64 {
    15
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 5990,
            token: "CHANGE-ME-TO-A-LONG-RANDOM-TOKEN".to_string(),
            network_adapters: Vec::new(),
            default_reboot_seconds: 15,
            hotkey: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub host: String,
    pub port: u16,
}

pub type Peers = BTreeMap<String, PeerInfo>;
pub type BlockedState = BTreeMap<String, String>;

fn base_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn config_path() -> PathBuf {
    base_dir().join("config.json")
}

pub fn peers_path() -> PathBuf {
    base_dir().join("peers.json")
}

fn state_path() -> PathBuf {
    base_dir().join("blocked_state.json")
}

pub fn log_path() -> PathBuf {
    base_dir().join("peer.log")
}

/// Загружает JSON-файл, создавая его с содержимым `default`, если он отсутствует.
pub fn load_json<T>(path: &Path, default: T) -> T
where
    T: Serialize + serde::de::DeserializeOwned,
{
    if !path.exists() {
        let text = serde_json::to_string_pretty(&default).expect("serialize default");
        let _ = std::fs::write(path, text);
        return default;
    }
    let text = std::fs::read_to_string(path).unwrap_or_else(|exc| {
        log_error!("failed to read {} -> {exc}", path.display());
        process::exit(1);
    });
    serde_json::from_str(&text).unwrap_or_else(|exc| {
        log_error!("failed to parse {} -> {exc}", path.display());
        process::exit(1);
    })
}

pub fn load_config() -> Config {
    let path = config_path();
    if !path.exists() {
        log_warn!(
            "No config.json found, creating a default one at {}. Edit it and set a real token before use.",
            path.display()
        );
    }
    let cfg: Config = load_json(&path, Config::default());
    if cfg.token == Config::default().token {
        log_warn!("Using the default placeholder token — set a real shared token in config.json");
    }
    cfg
}

pub fn save_config(cfg: &Config) {
    let text = serde_json::to_string_pretty(cfg).expect("serialize config");
    let _ = std::fs::write(config_path(), text);
}

pub fn default_peers() -> Peers {
    let mut peers = Peers::new();
    peers.insert(
        "example-pc".to_string(),
        PeerInfo {
            host: "26.0.0.1".to_string(),
            port: 5990,
        },
    );
    peers
}

pub fn load_state() -> BlockedState {
    let path = state_path();
    if !path.exists() {
        return BlockedState::new();
    }
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save_state(state: &BlockedState) {
    let text = serde_json::to_string_pretty(state).expect("serialize state");
    let _ = std::fs::write(state_path(), text);
}
