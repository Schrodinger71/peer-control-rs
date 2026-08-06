// клиентская часть: общение с другими узлами (из старого admin)

use crate::app::theme::{COLOR_GREEN, COLOR_RED};
use crate::config::PeerInfo;
use eframe::egui::Color32;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

pub fn send_command(
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

pub fn response_ok(resp: &Value) -> bool {
    resp.get("ok").and_then(Value::as_bool).unwrap_or(false)
}

pub fn response_message(resp: &Value) -> String {
    match resp.get("message") {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

pub fn ping_status(peer: &PeerInfo, token: &str) -> (String, Color32) {
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
