// логирование (файл + кольцевой буфер в памяти, показывается внизу GUI)
use std::collections::VecDeque;
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

static LOG_FILE: OnceLock<Mutex<std::fs::File>> = OnceLock::new();
static LOG_BUFFER: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();

const LOG_BUFFER_CAPACITY: usize = 300;

pub fn log_buffer() -> &'static Mutex<VecDeque<String>> {
    LOG_BUFFER.get_or_init(|| Mutex::new(VecDeque::with_capacity(LOG_BUFFER_CAPACITY)))
}

pub fn init_logging(path: &Path) {
    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = LOG_FILE.set(Mutex::new(file));
    }
}

pub fn log_line(level: &str, msg: &str) {
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

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => { $crate::logging::log_line("INFO", &format!($($arg)*)) };
}
#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => { $crate::logging::log_line("WARNING", &format!($($arg)*)) };
}
#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => { $crate::logging::log_line("ERROR", &format!($($arg)*)) };
}
