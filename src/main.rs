#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Единый узел (peer) для сети родительского контроля через файрвол.
//!
//! Объединяет то, что раньше было двумя отдельными программами (`agent` и
//! `admin`), в одну: каждый запущенный `peer.exe` одновременно является и
//! сервером (слушает TCP-порт, принимает команды block/unblock/status/
//! reboot_internet от других узлов), и клиентом с GUI (позволяет добавлять
//! другие узлы и отправлять им те же команды). Любой узел может отключить
//! интернет любому другому узлу; при этом `KILL_PROCESSES` (фиксированный
//! список) всегда принудительно завершается целиком, как дерево процессов, —
//! полностью симметрично.

use eframe::egui::{self, Color32, CornerRadius, RichText};
use global_hotkey::hotkey::{Code as HkCode, HotKey, Modifiers as HkModifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};
use std::{process, thread};

const DEFAULT_SECONDS: u64 = 15;
const MAX_FAILED_ATTEMPTS: usize = 5;
const FAILED_ATTEMPT_WINDOW: Duration = Duration::from_secs(60);
const LOG_BUFFER_CAPACITY: usize = 300;

/// Имена процессов, которые принудительно завершаются (как дерево процессов)
/// при каждом срабатывании "reboot_internet". Зашиты намертво - не
/// настраиваются во время работы программы.
const KILL_PROCESSES: &[&str] = &["GTA5_Enhanced.exe", "GTA5_Enhanced_BE.exe", "PlayGTAV.exe"];

const COLOR_GREEN: Color32 = Color32::from_rgb(0x4c, 0xaf, 0x50);
const COLOR_RED: Color32 = Color32::from_rgb(0xe5, 0x53, 0x53);
const COLOR_YELLOW: Color32 = Color32::from_rgb(0xff, 0xb7, 0x4d);
const COLOR_GRAY: Color32 = Color32::from_rgb(0x9e, 0x9e, 0x9e);
const COLOR_ACCENT: Color32 = Color32::from_rgb(0x5b, 0x8d, 0xef);
const COLOR_DANGER: Color32 = Color32::from_rgb(0xe0, 0x57, 0x57);

const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");

// логирование (файл + кольцевой буфер в памяти, показывается внизу GUI)
static LOG_FILE: OnceLock<Mutex<std::fs::File>> = OnceLock::new();
static LOG_BUFFER: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();

fn log_buffer() -> &'static Mutex<VecDeque<String>> {
    LOG_BUFFER.get_or_init(|| Mutex::new(VecDeque::with_capacity(LOG_BUFFER_CAPACITY)))
}

fn init_logging(path: &Path) {
    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = LOG_FILE.set(Mutex::new(file));
    }
}

fn log_line(level: &str, msg: &str) {
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let line = format!("{now} {level} {msg}");
    println!("{line}");
    if let Some(file) = LOG_FILE.get()
        && let Ok(mut f) = file.lock()
    {
        let _ = writeln!(f, "{line}");
    }
    if let Ok(mut buf) = log_buffer().lock() {
        if buf.len() >= LOG_BUFFER_CAPACITY {
            buf.pop_front();
        }
        buf.push_back(line);
    }
}

macro_rules! log_info {
    ($($arg:tt)*) => { log_line("INFO", &format!($($arg)*)) };
}
macro_rules! log_warn {
    ($($arg:tt)*) => { log_line("WARNING", &format!($($arg)*)) };
}
macro_rules! log_error {
    ($($arg:tt)*) => { log_line("ERROR", &format!($($arg)*)) };
}

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

// конфиг / узлы / сохранение состояния
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Config {
    /// Адрес/порт, на котором слушает собственный сервер этого узла.
    host: String,
    port: u16,
    /// Общий секрет: одинаковый у всех узлов сети (используется и при приёме
    /// запросов, и при их отправке другим).
    token: String,
    /// Название(я) сетевого адаптера, который дёргается при "reboot_internet".
    /// Оставьте пустым (по умолчанию), чтобы автоматически определять все
    /// физические Wi-Fi/Ethernet адаптеры - результат записывается сюда же,
    /// чтобы он был виден и стабилен.
    #[serde(default)]
    network_adapters: Vec<String>,
    #[serde(default = "default_reboot_seconds")]
    default_reboot_seconds: u64,
    /// Глобальная горячая клавиша, которая мгновенно запускает "перезагрузить
    /// интернет у всех" из GUI этого узла. Работает даже без фокуса окна.
    #[serde(default)]
    hotkey: Option<HotKey>,
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
struct PeerInfo {
    host: String,
    port: u16,
}

type Peers = BTreeMap<String, PeerInfo>;
type BlockedState = BTreeMap<String, String>;

fn base_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn config_path() -> PathBuf {
    base_dir().join("config.json")
}

fn peers_path() -> PathBuf {
    base_dir().join("peers.json")
}

fn state_path() -> PathBuf {
    base_dir().join("blocked_state.json")
}

fn log_path() -> PathBuf {
    base_dir().join("peer.log")
}

/// Загружает JSON-файл, создавая его с содержимым `default`, если он отсутствует.
fn load_json<T>(path: &Path, default: T) -> T
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

fn load_config() -> Config {
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

fn save_config(cfg: &Config) {
    let text = serde_json::to_string_pretty(cfg).expect("serialize config");
    let _ = std::fs::write(config_path(), text);
}

fn default_peers() -> Peers {
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

fn load_state() -> BlockedState {
    let path = state_path();
    if !path.exists() {
        return BlockedState::new();
    }
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_state(state: &BlockedState) {
    let text = serde_json::to_string_pretty(state).expect("serialize state");
    let _ = std::fs::write(state_path(), text);
}

// серверная часть: файрвол / адаптеры / завершение дерева процессов (из старого agent)
fn normalize_name(name: &str) -> String {
    let mut name = name.trim().to_lowercase();
    if !name.ends_with(".exe") {
        name.push_str(".exe");
    }
    name
}

/// Находит путь к исполняемому файлу по имени процесса: сначала среди
/// запущенных процессов, затем в разделе реестра App Paths для установленных
/// приложений.
fn resolve_path(process_name: &str) -> Option<String> {
    let mut system = sysinfo::System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    for proc in system.processes().values() {
        let name = proc.name().to_string_lossy().to_lowercase();
        if name == process_name
            && let Some(exe) = proc.exe()
        {
            return Some(exe.to_string_lossy().to_string());
        }
    }

    let key_path = format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{process_name}");
    for hive in [
        winreg::enums::HKEY_LOCAL_MACHINE,
        winreg::enums::HKEY_CURRENT_USER,
    ] {
        let root = winreg::RegKey::predef(hive);
        if let Ok(key) = root.open_subkey(&key_path)
            && let Ok(value) = key.get_value::<String, _>("")
            && !value.is_empty()
        {
            return Some(value);
        }
    }

    None
}

/// Автоматически определяет физические Wi-Fi и Ethernet адаптеры (например,
/// "Wi-Fi", "Беспроводная сеть", "Ethernet"), независимо от локали или того,
/// как пользователь их переименовал. Фильтрует по `PhysicalMediaType`/
/// `Virtual`, а не по имени, поэтому не захватывает виртуальные/туннельные
/// адаптеры (Radmin VPN, Hyper-V, WireGuard, VirtualBox host-only, ...),
/// которые тоже могут отчитываться как "802.3".
fn discover_adapters() -> Vec<String> {
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; \
             Get-NetAdapter | Where-Object { \
                 $_.Virtual -eq $false -and \
                 $_.PhysicalMediaType -in @('802.3', 'Native 802.11') -and \
                 $_.InterfaceDescription -notmatch 'VPN|TAP|Virtual|Tunnel|Loopback' \
             } | Select-Object -ExpandProperty Name",
        ])
        .output();

    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect(),
        Ok(out) => {
            log_warn!(
                "adapter auto-detection failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
            Vec::new()
        }
        Err(exc) => {
            log_warn!("adapter auto-detection failed: {exc}");
            Vec::new()
        }
    }
}

/// Имена процессов Windows, которые нельзя завершать через `kill_process_tree`.
/// Многие из них - защищённые ОС "критические процессы": Windows реагирует
/// на их завершение немедленным bugcheck'ом (BSOD), а не пытается продолжить
/// работу, и `taskkill /F` с правами admin/SYSTEM вполне может их убить.
/// Ни один из них никогда не является легитимным приложением для завершения
/// в целях родительского контроля.
const PROTECTED_PROCESS_NAMES: &[&str] = &[
    "smss.exe",
    "csrss.exe",
    "wininit.exe",
    "winlogon.exe",
    "services.exe",
    "lsass.exe",
    "lsaiso.exe",
    "svchost.exe",
    "dwm.exe",
    "explorer.exe",
    "fontdrvhost.exe",
    "sihost.exe",
    "taskhostw.exe",
    "spoolsv.exe",
    "audiodg.exe",
    "ntoskrnl.exe",
];

fn is_protected_process(name: &str) -> bool {
    PROTECTED_PROCESS_NAMES.contains(&normalize_name(name).as_str())
}

/// Принудительно завершает все запущенные процессы с этим именем образа,
/// вместе со всем деревом процессов (дети, внуки, ...), через `taskkill /T`.
fn kill_process_tree(name: &str) -> (bool, String) {
    if is_protected_process(name) {
        return (
            false,
            format!("refusing to kill protected system process '{name}'"),
        );
    }
    match Command::new("taskkill")
        .args(["/F", "/IM", name, "/T"])
        .output()
    {
        Ok(output) => {
            let ok = output.status.success();
            let text = if !output.stdout.is_empty() {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            } else {
                String::from_utf8_lossy(&output.stderr).trim().to_string()
            };
            (ok, text)
        }
        Err(exc) => (false, exc.to_string()),
    }
}

fn rule_names(process_name: &str) -> (String, String) {
    let safe = process_name.replace('"', "");
    (
        format!("PFC_block_{safe}_out"),
        format!("PFC_block_{safe}_in"),
    )
}

fn run_netsh(args: &[&str]) -> (bool, String) {
    match Command::new("netsh").args(args).output() {
        Ok(output) => {
            let ok = output.status.success();
            let text = if !output.stdout.is_empty() {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            } else {
                String::from_utf8_lossy(&output.stderr).trim().to_string()
            };
            (ok, text)
        }
        Err(exc) => (false, exc.to_string()),
    }
}

fn block_process(process_name: &str) -> (bool, String) {
    let process_name = normalize_name(process_name);
    let Some(path) = resolve_path(&process_name) else {
        return (
            false,
            format!(
                "could not resolve a path for '{process_name}' \
                 (not currently running and no installed App Paths entry found)"
            ),
        );
    };

    let (out_rule, in_rule) = rule_names(&process_name);
    let (ok1, msg1) = run_netsh(&[
        "advfirewall",
        "firewall",
        "add",
        "rule",
        &format!("name={out_rule}"),
        "dir=out",
        &format!("program={path}"),
        "action=block",
        "enable=yes",
    ]);
    let (ok2, msg2) = run_netsh(&[
        "advfirewall",
        "firewall",
        "add",
        "rule",
        &format!("name={in_rule}"),
        "dir=in",
        &format!("program={path}"),
        "action=block",
        "enable=yes",
    ]);
    if !(ok1 && ok2) {
        return (false, format!("netsh error: {msg1} / {msg2}"));
    }

    let mut state = load_state();
    state.insert(process_name.clone(), path.clone());
    save_state(&state);
    log_info!("blocked {process_name} ({path})");
    (true, path)
}

fn unblock_process(process_name: &str) -> (bool, String) {
    let process_name = normalize_name(process_name);
    let (out_rule, in_rule) = rule_names(&process_name);
    run_netsh(&[
        "advfirewall",
        "firewall",
        "delete",
        "rule",
        &format!("name={out_rule}"),
    ]);
    run_netsh(&[
        "advfirewall",
        "firewall",
        "delete",
        "rule",
        &format!("name={in_rule}"),
    ]);

    let mut state = load_state();
    state.remove(&process_name);
    save_state(&state);
    log_info!("unblocked {process_name}");
    (true, "unblocked".to_string())
}

/// Ненадолго отключает целевой(ые) сетевой(ые) адаптер(ы), затем включает их
/// обратно автоматически. Самовосстановление заложено в саму конструкцию:
/// восстановление - это локальный таймер, а не вторая команда, поэтому оно
/// не зависит от того, переживёт ли канал управления обрыв связи. Также
/// принудительно завершает `KILL_PROCESSES` (каждый как дерево процессов) в
/// тот же момент, когда пропадает сеть.
///
/// Если `network_adapters` не настроены, физические Wi-Fi и Ethernet
/// адаптеры определяются автоматически, и дёргаются все сразу.
fn reboot_internet(cfg: &Arc<RwLock<Config>>, seconds: u64) -> (bool, String) {
    let snapshot = cfg.read().unwrap().clone();
    let mut adapters = snapshot.network_adapters;
    let auto_detected = adapters.is_empty();
    if auto_detected {
        adapters = discover_adapters();
    }
    if adapters.is_empty() {
        return (
            false,
            "no Wi-Fi/Ethernet adapters could be auto-detected; configure them \
             explicitly via \"network_adapters\" in config.json, \
             e.g. \"network_adapters\": [\"Wi-Fi\"] \
             (find the exact name with: netsh interface show interface)"
                .to_string(),
        );
    }
    if auto_detected {
        log_info!("auto-detected adapters {adapters:?}, saving to config.json");
        let mut guard = cfg.write().unwrap();
        guard.network_adapters = adapters.clone();
        save_config(&guard);
    }

    let adapters_for_thread = adapters.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(500)); // даём ack ниже дойти до отправителя, пока канал ещё жив
        for name in KILL_PROCESSES {
            let (ok, msg) = kill_process_tree(name);
            log_info!("kill_process_tree({name}) -> ok={ok} {msg}");
        }
        for name in &adapters_for_thread {
            run_netsh(&[
                "interface",
                "set",
                "interface",
                &format!("name={name}"),
                "admin=disable",
            ]);
        }
        log_info!("disabled adapters {adapters_for_thread:?} for {seconds}s");
        thread::sleep(Duration::from_secs(seconds));
        for name in &adapters_for_thread {
            run_netsh(&[
                "interface",
                "set",
                "interface",
                &format!("name={name}"),
                "admin=enable",
            ]);
        }
        log_info!("re-enabled adapters {adapters_for_thread:?}");
    });

    (
        true,
        format!("disabling {adapters:?} now, will restore automatically after {seconds}s"),
    )
}

// ограничение частоты попыток 
static FAILED_ATTEMPTS: OnceLock<Mutex<HashMap<IpAddr, Vec<Instant>>>> = OnceLock::new();

fn failed_attempts() -> &'static Mutex<HashMap<IpAddr, Vec<Instant>>> {
    FAILED_ATTEMPTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Возвращает true, если этому узлу сейчас разрешено попытаться пройти авторизацию.
fn check_rate_limit(peer_ip: IpAddr) -> bool {
    let now = Instant::now();
    let mut attempts = failed_attempts().lock().unwrap();
    let entry = attempts.entry(peer_ip).or_default();
    entry.retain(|t| now.duration_since(*t) < FAILED_ATTEMPT_WINDOW);
    entry.len() < MAX_FAILED_ATTEMPTS
}

fn record_failed_attempt(peer_ip: IpAddr) {
    let mut attempts = failed_attempts().lock().unwrap();
    attempts.entry(peer_ip).or_default().push(Instant::now());
}

/// Побайтовое сравнение за постоянное время (аналог Python-функции
/// hmac.compare_digest), чтобы общий токен не утёк через тайминг сравнения.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// обработка входящих запросов
fn handle_client(
    mut conn: TcpStream,
    peer_ip: IpAddr,
    cfg: &Arc<RwLock<Config>>,
    ctx: &egui::Context,
    tx: &Sender<StatusEvent>,
) {
    let timeout = Duration::from_secs(10);
    let _ = conn.set_read_timeout(Some(timeout));
    let _ = conn.set_write_timeout(Some(timeout));

    let mut reader = BufReader::new(conn.try_clone().expect("clone tcp stream"));
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }

    let respond = |conn: &mut TcpStream, response: &Value| {
        let text = serde_json::to_string(response).unwrap_or_else(|_| "{}".to_string());
        let _ = conn.write_all(format!("{text}\n").as_bytes());
    };

    if !check_rate_limit(peer_ip) {
        log_warn!("rate-limited {peer_ip} (too many failed auth attempts)");
        respond(&mut conn, &json!({"ok": false, "message": "rate limited"}));
        ctx.request_repaint();
        return;
    }

    let request: Value = match serde_json::from_str(line.trim_end()) {
        Ok(value) => value,
        Err(_) => {
            respond(&mut conn, &json!({"ok": false, "message": "bad request"}));
            ctx.request_repaint();
            return;
        }
    };

    let supplied = request.get("token").and_then(Value::as_str).unwrap_or("");
    let token = cfg.read().unwrap().token.clone();
    if !constant_time_eq(supplied.as_bytes(), token.as_bytes()) {
        record_failed_attempt(peer_ip);
        log_warn!("auth failed from {peer_ip}");
        respond(&mut conn, &json!({"ok": false, "message": "unauthorized"}));
        ctx.request_repaint();
        return;
    }

    let action = request.get("action").and_then(Value::as_str).unwrap_or("");
    let response = match action {
        "ping" => {
            let hostname = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "unknown".to_string());
            json!({"ok": true, "message": hostname})
        }
        "status" => {
            json!({"ok": true, "message": {"blocked_processes": load_state(), "kill_processes": KILL_PROCESSES}})
        }
        "block" => {
            let process = request.get("process").and_then(Value::as_str).unwrap_or("");
            let (ok, msg) = block_process(process);
            json!({"ok": ok, "message": msg})
        }
        "unblock" => {
            let process = request.get("process").and_then(Value::as_str).unwrap_or("");
            let (ok, msg) = unblock_process(process);
            json!({"ok": ok, "message": msg})
        }
        "reboot_internet" => {
            let seconds = request
                .get("seconds")
                .and_then(Value::as_u64)
                .unwrap_or_else(|| cfg.read().unwrap().default_reboot_seconds);
            let (ok, msg) = reboot_internet(cfg, seconds);
            if ok {
                let _ = tx.send(StatusEvent::Toast {
                    message: format!("⚠ {peer_ip} отключил(а) вам интернет на {seconds} сек"),
                });
            }
            json!({"ok": ok, "message": msg})
        }
        other => json!({"ok": false, "message": format!("unknown action '{other}'")}),
    };

    log_info!("{action} from {peer_ip} -> {response}");
    respond(&mut conn, &response);
    ctx.request_repaint();
}

/// Запускает TCP-сервер (цикл приёма соединений в фоновом потоке). Каждое
/// входящее соединение обрабатывается в своём потоке, как и в отдельном agent.
fn spawn_server(cfg: Arc<RwLock<Config>>, ctx: egui::Context, tx: Sender<StatusEvent>) {
    let (host, port) = {
        let guard = cfg.read().unwrap();
        (guard.host.clone(), guard.port)
    };

    let listener = match TcpListener::bind((host.as_str(), port)) {
        Ok(listener) => listener,
        Err(exc) => {
            log_error!("failed to bind {host}:{port} -> {exc}");
            return;
        }
    };
    log_info!("listening on {host}:{port}");

    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let Ok(peer_addr) = stream.peer_addr() else {
                continue;
            };
            let cfg = cfg.clone();
            let ctx = ctx.clone();
            let tx = tx.clone();
            thread::spawn(move || {
                handle_client(stream, peer_addr.ip(), &cfg, &ctx, &tx);
            });
        }
    });
}

// клиентская часть: общение с другими узлами (из старого admin)
fn send_command(
    host: &str,
    port: u16,
    token: &str,
    request: Value,
    timeout: Duration,
) -> std::io::Result<Value> {
    let addr = (host, port).to_socket_addrs()?.next().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "could not resolve address")
    })?;
    let stream = TcpStream::connect_timeout(&addr, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;

    let mut payload = request;
    payload["token"] = Value::String(token.to_string());
    let line = serde_json::to_string(&payload)? + "\n";
    (&stream).write_all(line.as_bytes())?;

    let mut reader = BufReader::new(&stream);
    let mut response_line = String::new();
    reader.read_line(&mut response_line)?;
    serde_json::from_str(response_line.trim_end())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

fn response_ok(resp: &Value) -> bool {
    resp.get("ok").and_then(Value::as_bool).unwrap_or(false)
}

fn response_message(resp: &Value) -> String {
    match resp.get("message") {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

fn ping_status(peer: &PeerInfo, token: &str) -> (String, Color32) {
    let start = Instant::now();
    match send_command(
        &peer.host,
        peer.port,
        token,
        json!({"action": "ping"}),
        Duration::from_secs_f32(4.0),
    ) {
        Ok(resp) if response_ok(&resp) => {
            let ms = start.elapsed().as_millis();
            (format!("в сети ({ms} мс)"), COLOR_GREEN)
        }
        Ok(_) => ("ошибка".to_string(), COLOR_RED),
        Err(_) => ("недоступен".to_string(), COLOR_RED),
    }
}

// вспомогательные функции UI / оформление
fn decode_icon() -> (Vec<u8>, u32, u32) {
    let image = image::load_from_memory(ICON_PNG)
        .expect("decode embedded icon")
        .into_rgba8();
    let (width, height) = image.dimensions();
    (image.into_raw(), width, height)
}

/// Более спокойная и скруглённая тема, чем стандартная тёмная тема egui.
fn build_visuals() -> egui::Visuals {
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
fn status_badge(ui: &mut egui::Ui, text: &str, color: Color32) {
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

// фоновые события статуса
enum StatusEvent {
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

// всплывающие уведомления в приложении
struct Toast {
    message: String,
    created_at: Instant,
}

const TOAST_LIFETIME: Duration = Duration::from_secs(6);

// состояние диалога добавления узла
struct AddDialog {
    name: String,
    host: String,
    port: String,
    error: Option<String>,
}

impl AddDialog {
    fn new() -> Self {
        Self {
            name: String::new(),
            host: String::new(),
            port: "5990".to_string(),
            error: None,
        }
    }
}

// состояние диалога захвата горячей клавиши
struct HotkeyCapture {
    captured: Option<HotKey>,
}

impl HotkeyCapture {
    fn new() -> Self {
        Self { captured: None }
    }
}

/// Преобразует клавишу egui в физический код клавиши `global_hotkey`.
/// Покрывает буквы, цифры, функциональные клавиши и основные клавиши
/// навигации/редактирования - этого достаточно для любой разумной комбинации.
fn egui_key_to_code(key: egui::Key) -> Option<HkCode> {
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

fn egui_modifiers_to_hk(modifiers: egui::Modifiers) -> Option<HkModifiers> {
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


// приложение
struct PeerApp {
    cfg: Arc<RwLock<Config>>,
    peers: Peers,
    statuses: BTreeMap<String, (String, Color32)>,
    seconds_input: String,
    ctx: egui::Context,
    tx: Sender<StatusEvent>,
    rx: Receiver<StatusEvent>,
    add_dialog: Option<AddDialog>,
    confirm_remove: Option<String>,
    errors: Vec<(String, String)>,
    icon_texture: egui::TextureHandle,
    hotkey_manager: Option<GlobalHotKeyManager>,
    active_hotkey: Option<HotKey>,
    hotkey_active_id: Arc<Mutex<Option<u32>>>,
    hotkey_capture: Option<HotkeyCapture>,
    toasts: Vec<Toast>,
}

impl PeerApp {
    fn new(cc: &eframe::CreationContext<'_>, cfg: Arc<RwLock<Config>>, peers: Peers) -> Self {
        cc.egui_ctx.set_visuals(build_visuals());

        let (tx, rx) = mpsc::channel();

        spawn_server(cfg.clone(), cc.egui_ctx.clone(), tx.clone());

        let (rgba, width, height) = decode_icon();
        let color_image =
            egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &rgba);
        let icon_texture =
            cc.egui_ctx
                .load_texture("app_icon", color_image, egui::TextureOptions::LINEAR);

        let hotkey_manager = GlobalHotKeyManager::new().ok();
        let hotkey_active_id: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
        let mut active_hotkey = None;
        let mut startup_error = None;
        let saved_hotkey = cfg.read().unwrap().hotkey;
        if let (Some(manager), Some(saved)) = (&hotkey_manager, saved_hotkey) {
            match manager.register(saved) {
                Ok(()) => {
                    active_hotkey = Some(saved);
                    *hotkey_active_id.lock().unwrap() = Some(saved.id);
                }
                Err(exc) => {
                    startup_error = Some((
                        "Горячие клавиши".to_string(),
                        format!("Не удалось назначить сохранённую комбинацию «{saved}»: {exc}"),
                    ));
                }
            }
        }

        {
            let hotkey_active_id = hotkey_active_id.clone();
            let tx = tx.clone();
            let ctx = cc.egui_ctx.clone();
            thread::spawn(move || {
                let receiver = GlobalHotKeyEvent::receiver();
                while let Ok(event) = receiver.recv() {
                    if event.state() != HotKeyState::Pressed {
                        continue;
                    }
                    let matches = hotkey_active_id
                        .lock()
                        .is_ok_and(|guard| *guard == Some(event.id()));
                    if matches {
                        let _ = tx.send(StatusEvent::HotkeyTriggered);
                        ctx.request_repaint();
                    }
                }
            });
        }

        let mut app = Self {
            cfg,
            peers,
            statuses: BTreeMap::new(),
            seconds_input: DEFAULT_SECONDS.to_string(),
            ctx: cc.egui_ctx.clone(),
            tx,
            rx,
            add_dialog: None,
            confirm_remove: None,
            errors: Vec::new(),
            icon_texture,
            hotkey_manager,
            active_hotkey,
            hotkey_active_id,
            hotkey_capture: None,
            toasts: Vec::new(),
        };
        if let Some(error) = startup_error {
            app.errors.push(error);
        }
        app.refresh_all_statuses();
        app
    }

    // ---------- сохранение данных ----------

    fn token(&self) -> String {
        self.cfg.read().unwrap().token.clone()
    }

    fn save_peers(&self) {
        let text = serde_json::to_string_pretty(&self.peers).expect("serialize peers");
        std::fs::write(peers_path(), text).expect("write peers.json");
    }

    fn save_config(&self) {
        save_config(&self.cfg.read().unwrap());
    }

    // ---------- горячая клавиша ----------

    fn set_hotkey(&mut self, hotkey: HotKey) {
        let Some(manager) = &self.hotkey_manager else {
            self.errors.push((
                "Горячие клавиши".to_string(),
                "Менеджер горячих клавиш не инициализирован.".to_string(),
            ));
            return;
        };
        if let Some(old) = self.active_hotkey.take() {
            let _ = manager.unregister(old);
        }
        match manager.register(hotkey) {
            Ok(()) => {
                self.active_hotkey = Some(hotkey);
                if let Ok(mut guard) = self.hotkey_active_id.lock() {
                    *guard = Some(hotkey.id);
                }
                self.cfg.write().unwrap().hotkey = Some(hotkey);
                self.save_config();
            }
            Err(exc) => {
                self.errors.push((
                    "Горячие клавиши".to_string(),
                    format!("Не удалось назначить комбинацию «{hotkey}»: {exc}"),
                ));
            }
        }
    }

    fn clear_hotkey(&mut self) {
        if let Some(manager) = &self.hotkey_manager
            && let Some(old) = self.active_hotkey.take()
        {
            let _ = manager.unregister(old);
        }
        if let Ok(mut guard) = self.hotkey_active_id.lock() {
            *guard = None;
        }
        self.cfg.write().unwrap().hotkey = None;
        self.save_config();
    }

    // ---------- действия ----------

    fn seconds(&self) -> u64 {
        self.seconds_input
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|&s| s >= 1)
            .unwrap_or(DEFAULT_SECONDS)
    }

    fn refresh_all_statuses(&mut self) {
        for name in self.peers.keys().cloned().collect::<Vec<_>>() {
            self.spawn_ping(name);
        }
    }

    fn spawn_ping(&self, name: String) {
        let Some(peer) = self.peers.get(&name).cloned() else {
            return;
        };
        let token = self.token();
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        thread::spawn(move || {
            let (text, color) = ping_status(&peer, &token);
            let _ = tx.send(StatusEvent::Status { name, text, color });
            ctx.request_repaint();
        });
    }

    fn reboot_one(&mut self, name: String) {
        let seconds = self.seconds();
        self.statuses
            .insert(name.clone(), ("перезагрузка...".to_string(), COLOR_YELLOW));
        self.spawn_reboot(name, seconds);
    }

    fn spawn_reboot(&self, name: String, seconds: u64) {
        let Some(peer) = self.peers.get(&name).cloned() else {
            return;
        };
        let token = self.token();
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        log_info!(
            "sending reboot_internet to {name} ({}:{}) for {seconds}s",
            peer.host,
            peer.port
        );
        thread::spawn(move || {
            let result = send_command(
                &peer.host,
                peer.port,
                &token,
                json!({"action": "reboot_internet", "seconds": seconds}),
                Duration::from_secs_f32(6.0),
            );
            match result {
                Ok(resp) if response_ok(&resp) => {
                    log_info!("reboot_internet to {name} -> ok");
                    let _ = tx.send(StatusEvent::Status {
                        name: name.clone(),
                        text: format!("пауза {seconds}с"),
                        color: COLOR_YELLOW,
                    });
                    ctx.request_repaint();

                    thread::sleep(Duration::from_millis(seconds * 1000 + 1500));
                    let (text, color) = ping_status(&peer, &token);
                    let _ = tx.send(StatusEvent::Status { name, text, color });
                    ctx.request_repaint();
                }
                Ok(resp) => {
                    let message = response_message(&resp);
                    log_warn!("reboot_internet to {name} -> {message}");
                    let _ = tx.send(StatusEvent::Error {
                        title: name.clone(),
                        message,
                    });
                    let _ = tx.send(StatusEvent::Status {
                        name,
                        text: "ошибка".to_string(),
                        color: COLOR_RED,
                    });
                    ctx.request_repaint();
                }
                Err(exc) => {
                    log_warn!("reboot_internet to {name} -> could not connect: {exc}");
                    let _ = tx.send(StatusEvent::Error {
                        title: name.clone(),
                        message: format!("Не удалось подключиться: {exc}"),
                    });
                    let _ = tx.send(StatusEvent::Status {
                        name,
                        text: "недоступен".to_string(),
                        color: COLOR_RED,
                    });
                    ctx.request_repaint();
                }
            }
        });
    }

    fn reboot_all(&mut self) {
        let seconds = self.seconds();
        for name in self.peers.keys().cloned().collect::<Vec<_>>() {
            self.statuses
                .insert(name.clone(), ("перезагрузка...".to_string(), COLOR_YELLOW));
            self.spawn_reboot(name, seconds);
        }
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                StatusEvent::Status { name, text, color } => {
                    self.statuses.insert(name, (text, color));
                }
                StatusEvent::Error { title, message } => {
                    self.errors.push((title, message));
                }
                StatusEvent::HotkeyTriggered => {
                    self.reboot_all();
                }
                StatusEvent::Toast { message } => {
                    self.toasts.push(Toast {
                        message,
                        created_at: Instant::now(),
                    });
                }
            }
        }
    }

    // ---------- элементы UI ----------

    fn show_row(&mut self, ui: &mut egui::Ui, name: &str) {
        let peer = self.peers[name].clone();
        let (status_text, status_color) = self
            .statuses
            .get(name)
            .cloned()
            .unwrap_or_else(|| ("проверка...".to_string(), COLOR_GRAY));

        egui::Frame::new()
            .fill(ui.visuals().faint_bg_color)
            .corner_radius(CornerRadius::same(10))
            .inner_margin(egui::Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(name).strong().size(15.0));
                        ui.label(
                            RichText::new(format!("{}:{}", peer.host, peer.port))
                                .color(ui.visuals().weak_text_color())
                                .size(12.0),
                        );
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Удалить").clicked() {
                            self.confirm_remove = Some(name.to_string());
                        }
                        if ui
                            .add(
                                egui::Button::new("Перезагрузить")
                                    .fill(COLOR_ACCENT.linear_multiply(0.25)),
                            )
                            .clicked()
                        {
                            self.reboot_one(name.to_string());
                        }
                        ui.add_space(4.0);
                        status_badge(ui, &status_text, status_color);
                    });
                });
            });
        ui.add_space(6.0);
    }

    fn show_add_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.add_dialog else {
            return;
        };
        let mut close = false;
        let mut submit = false;

        egui::Modal::new(egui::Id::new("add_peer_modal")).show(ctx, |ui| {
            ui.set_min_width(320.0);
            ui.heading("Новый узел сети");
            ui.add_space(8.0);

            egui::Grid::new("add_peer_grid")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label("Название:");
                    ui.text_edit_singleline(&mut dialog.name);
                    ui.end_row();

                    ui.label("RadminVPN-адрес (host):");
                    ui.text_edit_singleline(&mut dialog.host);
                    ui.end_row();

                    ui.label("Порт:");
                    ui.text_edit_singleline(&mut dialog.port);
                    ui.end_row();
                });

            if let Some(err) = &dialog.error {
                ui.add_space(6.0);
                ui.colored_label(COLOR_RED, err);
            }

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Добавить").clicked() {
                    submit = true;
                }
                if ui.button("Отмена").clicked() {
                    close = true;
                }
            });
        });

        if submit {
            let name = dialog.name.trim().to_string();
            let host = dialog.host.trim().to_string();
            let port: Option<u16> = dialog.port.trim().parse().ok();

            if name.is_empty() {
                dialog.error = Some("Введите название компьютера.".to_string());
            } else if self.peers.contains_key(&name) {
                dialog.error = Some("Узел с таким названием уже добавлен.".to_string());
            } else if host.is_empty() {
                dialog.error = Some("Введите RadminVPN-адрес.".to_string());
            } else if port.is_none() {
                dialog.error = Some("Введите корректный порт.".to_string());
            } else {
                self.peers.insert(
                    name.clone(),
                    PeerInfo {
                        host,
                        port: port.unwrap(),
                    },
                );
                self.save_peers();
                self.add_dialog = None;
                self.spawn_ping(name);
                return;
            }
        }
        if close {
            self.add_dialog = None;
        }
    }

    fn show_confirm_remove(&mut self, ctx: &egui::Context) {
        let Some(name) = self.confirm_remove.clone() else {
            return;
        };
        let mut close = false;
        let mut remove = false;

        egui::Modal::new(egui::Id::new("confirm_remove_modal")).show(ctx, |ui| {
            ui.set_min_width(280.0);
            ui.label(format!("Удалить «{name}» из списка?"));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Да").clicked() {
                    remove = true;
                }
                if ui.button("Нет").clicked() {
                    close = true;
                }
            });
        });

        if remove {
            self.peers.remove(&name);
            self.statuses.remove(&name);
            self.save_peers();
            self.confirm_remove = None;
        } else if close {
            self.confirm_remove = None;
        }
    }

    fn show_hotkey_capture(&mut self, ctx: &egui::Context) {
        let Some(capture) = &mut self.hotkey_capture else {
            return;
        };

        ctx.input(|i| {
            for event in &i.events {
                if let egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    repeat: false,
                    ..
                } = event
                    && let Some(code) = egui_key_to_code(*key)
                {
                    capture.captured = Some(HotKey::new(egui_modifiers_to_hk(*modifiers), code));
                }
            }
        });

        let mut close = false;
        let mut submit = false;

        egui::Modal::new(egui::Id::new("hotkey_capture_modal")).show(ctx, |ui| {
            ui.set_min_width(340.0);
            ui.heading("Горячая клавиша для «Перезагрузить у всех»");
            ui.add_space(8.0);
            match capture.captured {
                Some(hk) => {
                    ui.label(RichText::new(hk.to_string()).strong().size(16.0));
                }
                None => {
                    ui.label("Нажмите нужную комбинацию клавиш (например, Ctrl+Shift+5)...");
                }
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(capture.captured.is_some(), egui::Button::new("Сохранить"))
                    .clicked()
                {
                    submit = true;
                }
                if ui.button("Отмена").clicked() {
                    close = true;
                }
            });
        });

        let captured = capture.captured;

        if submit {
            if let Some(hotkey) = captured {
                self.set_hotkey(hotkey);
            }
            self.hotkey_capture = None;
        } else if close {
            self.hotkey_capture = None;
        }
    }

    fn show_toasts(&mut self, ctx: &egui::Context) {
        self.toasts
            .retain(|toast| toast.created_at.elapsed() < TOAST_LIFETIME);

        for (index, toast) in self.toasts.iter().enumerate() {
            egui::Area::new(egui::Id::new(("toast", index)))
                .anchor(
                    egui::Align2::RIGHT_TOP,
                    egui::vec2(-16.0, 16.0 + index as f32 * 56.0),
                )
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::Frame::new()
                        .fill(COLOR_ACCENT.linear_multiply(0.9))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(egui::Margin::symmetric(14, 10))
                        .show(ui, |ui| {
                            ui.set_max_width(320.0);
                            ui.label(RichText::new(&toast.message).color(Color32::WHITE));
                        });
                });
        }

        if !self.toasts.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    fn show_error_dialog(&mut self, ctx: &egui::Context) {
        let Some((title, message)) = self.errors.first().cloned() else {
            return;
        };
        let mut close = false;

        egui::Modal::new(egui::Id::new("error_modal")).show(ctx, |ui| {
            ui.set_min_width(300.0);
            ui.heading(&title);
            ui.add_space(8.0);
            ui.label(&message);
            ui.add_space(10.0);
            if ui.button("OK").clicked() {
                close = true;
            }
        });

        if close {
            self.errors.remove(0);
        }
    }

    fn show_log_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Журнал").strong());
            ui.label(
                RichText::new("(и входящие, и исходящие команды)")
                    .color(ui.visuals().weak_text_color())
                    .size(11.0),
            );
        });
        ui.add_space(4.0);
        egui::Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .max_height(440.0)
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        if let Ok(buf) = log_buffer().lock() {
                            for line in buf.iter() {
                                ui.label(RichText::new(line).monospace().size(11.0));
                            }
                        }
                    });
            });
    }
}

impl eframe::App for PeerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_events();
        let ctx = ui.ctx().clone();

        egui::Panel::top("top_bar")
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::symmetric(16, 14)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::Image::new((
                        self.icon_texture.id(),
                        egui::vec2(28.0, 28.0),
                    )));
                    ui.add_space(8.0);
                    ui.heading(RichText::new("Networked Program Peer").strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new("+ Добавить узел")
                                    .fill(COLOR_ACCENT.linear_multiply(0.35)),
                            )
                            .clicked()
                        {
                            self.add_dialog = Some(AddDialog::new());
                        }
                        if ui.button("↻ Обновить статус").clicked() {
                            self.refresh_all_statuses();
                        }
                    });
                });
            });

        egui::Panel::bottom("log_panel")
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::symmetric(16, 10)),
            )
            .default_size(480.0)
            .show(ui, |ui| {
                self.show_log_panel(ui);
            });

        egui::Panel::bottom("controls_bar")
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::symmetric(16, 12)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Длительность отключения (сек):");
                    ui.add(egui::TextEdit::singleline(&mut self.seconds_input).desired_width(50.0));
                    ui.add_space(12.0);
                    ui.label("Горячая клавиша:");
                    match self.active_hotkey {
                        Some(hk) => {
                            ui.label(RichText::new(hk.to_string()).strong());
                            if ui.small_button("✕").clicked() {
                                self.clear_hotkey();
                            }
                        }
                        None => {
                            ui.label(
                                RichText::new("не назначена").color(ui.visuals().weak_text_color()),
                            );
                        }
                    }
                    if ui.small_button("Назначить").clicked() {
                        self.hotkey_capture = Some(HotkeyCapture::new());
                    }
                });
                ui.add_space(8.0);
                let reboot_all_btn = egui::Button::new(
                    RichText::new("🔄 Перезагрузить интернет у всех").color(Color32::WHITE),
                )
                .fill(COLOR_DANGER)
                .corner_radius(CornerRadius::same(8));
                if ui
                    .add_sized([ui.available_width(), 34.0], reboot_all_btn)
                    .clicked()
                    && !self.peers.is_empty()
                {
                    self.reboot_all();
                }
                ui.add_space(4.0);
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().extreme_bg_color)
                    .inner_margin(egui::Margin::symmetric(14, 12)),
            )
            .show(ui, |ui| {
                if self.peers.is_empty() {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| {
                        ui.label("Нет добавленных узлов. Нажмите «+ Добавить узел».");
                    });
                    return;
                }

                egui::ScrollArea::vertical().show(ui, |ui| {
                    for name in self.peers.keys().cloned().collect::<Vec<_>>() {
                        self.show_row(ui, &name);
                    }
                });
            });

        self.show_add_dialog(&ctx);
        self.show_confirm_remove(&ctx);
        self.show_hotkey_capture(&ctx);
        self.show_error_dialog(&ctx);
        self.show_toasts(&ctx);
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
        let adapters = discover_adapters();
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

    let (rgba, width, height) = decode_icon();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("MeshControl.exe")
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
        "peer-network-node",
        options,
        Box::new(move |cc| Ok(Box::new(PeerApp::new(cc, cfg, peers)))),
    );
    if let Err(exc) = run_result {
        log_error!("eframe run_native failed: {exc}");
    }
}
