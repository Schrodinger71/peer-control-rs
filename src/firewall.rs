// серверная часть: файрвол / адаптеры / завершение дерева процессов (из старого agent)
use std::process::Command;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

use crate::config::{Config, load_state, save_config, save_state};
use crate::{log_info, log_warn};

/// Имена процессов, которые принудительно завершаются (как дерево процессов)
/// при каждом срабатывании "reboot_internet". Зашиты намертво - не
/// настраиваются во время работы программы.
pub const KILL_PROCESSES: &[&str] = &["GTA5_Enhanced.exe", "GTA5_Enhanced_BE.exe", "PlayGTAV.exe"];

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
pub fn discover_adapters() -> Vec<String> {
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

pub fn block_process(process_name: &str) -> (bool, String) {
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

pub fn unblock_process(process_name: &str) -> (bool, String) {
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
pub fn reboot_internet(cfg: &Arc<RwLock<Config>>, seconds: u64) -> (bool, String) {
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
