// серверная часть: приём запросов, ограничение частоты попыток

use crate::app::events::StatusEvent;
use crate::config::{Config, load_state};
use crate::firewall::{KILL_PROCESSES, block_process, reboot_internet, unblock_process};
use crate::{log_error, log_info, log_warn};
use eframe::egui;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::thread;
use std::time::{Duration, Instant};
use winrt_notification::{Duration as ToastDuration, Sound, Toast as WinToast};

// ---------- ограничение частоты попыток ----------

const MAX_FAILED_ATTEMPTS: usize = 5;
const FAILED_ATTEMPT_WINDOW: Duration = Duration::from_secs(60);

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

// ---------- обработка входящих запросов ----------

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
                let format_string =
                    format!("⚠ {peer_ip} отключил(а) вам интернет на {seconds} сек");

                WinToast::new(WinToast::POWERSHELL_APP_ID)
                    .title("peer-control-rs")
                    .text1(&format_string)
                    .sound(Some(Sound::Default))
                    .duration(ToastDuration::Short)
                    .show()
                    .ok();

                let _ = tx.send(StatusEvent::Toast {
                    message: format_string,
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
pub fn spawn_server(cfg: Arc<RwLock<Config>>, ctx: egui::Context, tx: Sender<StatusEvent>) {
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
