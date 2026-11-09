//! adbpair - wireless adb pairing helper for the Honor BKQ-AN90 toolkit.
//!
//! Flow:
//!   1. if `adb devices` already shows a device in "device" state, hand it back
//!   2. otherwise show a QR code (`WIFI:T:ADB;S:debug;P:<code>;;`) in a window and
//!      retry `adb pair` until the phone scans it
//!   3. `adb connect` the device that appeared, then wait for it to be ready
//!
//! stdout carries exactly one line, `ADB_TARGET=<serial>` (empty on failure),
//! everything else goes to stderr so the calling batch file can parse stdout.

use std::collections::HashSet;
use std::env;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use minifb::{Key, KeyRepeat, Scale, Window, WindowOptions};
use qrcode::Color as QrColor;
use qrcode::QrCode;

const TITLE: &str = "ADB Pair - Scan with phone camera";
const SERVICE_NAME: &str = "debug";
const PAIR_SERVICE: &str = "_adb-tls-pairing._tcp";
const CONNECT_SERVICE: &str = "_adb-tls-connect._tcp";

const MODULE_PX: usize = 8;
const QUIET_MODULES: usize = 4;

const PAIR_TIMEOUT: Duration = Duration::from_secs(120);
const PAIR_INTERVAL: Duration = Duration::from_secs(2);
const PAIR_ATTEMPTS: u32 = 60;
const PAIR_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(20);

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_INTERVAL: Duration = Duration::from_secs(2);
const CONNECT_RETRY: Duration = Duration::from_secs(5);
const CONNECT_DISCOVERY: Duration = Duration::from_secs(8);

const DEVICE_STATES: &[&str] = &[
    "device",
    "offline",
    "unauthorized",
    "authorizing",
    "connecting",
    "recovery",
    "sideload",
    "bootloader",
    "host",
    "rescue",
];

fn main() {
    let adb = match find_adb() {
        Some(p) => p,
        None => {
            eprintln!(
                "[ERROR] adb not found (looked next to adbpair.exe, D:\\shuaji\\Tools and PATH)"
            );
            finish(None);
        }
    };
    eprintln!("[INFO] adb: {}", adb.display());

    if let Some(serial) = ready_device(&adb) {
        eprintln!("[INFO] device already connected: {serial}");
        finish(Some(serial));
    }

    eprintln!("[INFO] no device in 'device' state, starting wireless pairing");
    finish(run_pairing(&adb));
}

fn finish(target: Option<String>) -> ! {
    match target {
        Some(t) => {
            println!("ADB_TARGET={t}");
            let _ = std::io::stdout().flush();
            std::process::exit(0);
        }
        None => {
            println!("ADB_TARGET=");
            let _ = std::io::stdout().flush();
            std::process::exit(1);
        }
    }
}

// ---------------------------------------------------------------- adb lookup

fn find_adb() -> Option<PathBuf> {
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            let beside = dir.join("adb.exe");
            if beside.is_file() {
                return Some(beside);
            }
        }
    }

    let fallback = PathBuf::from(r"D:\shuaji\Tools\adb.exe");
    if fallback.is_file() {
        return Some(fallback);
    }

    if let Some(paths) = env::var_os("PATH") {
        for dir in env::split_paths(&paths) {
            for name in ["adb.exe", "adb"] {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }

    None
}

// ------------------------------------------------------------- adb plumbing

fn run_adb(adb: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new(adb)
        .env("ADB_MDNS_OPENSCREEN", "1")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Some(text)
}

/// Same as [`run_adb`], but pollable so a closed window aborts the child early.
fn run_adb_interruptible(
    adb: &Path,
    args: &[&str],
    stop: &AtomicBool,
    limit: Duration,
) -> Option<String> {
    let mut child = Command::new(adb)
        .env("ADB_MDNS_OPENSCREEN", "1")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;

    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
        if stop.load(Ordering::SeqCst) || started.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        thread::sleep(Duration::from_millis(100));
    }

    let out = child.wait_with_output().ok()?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Some(text)
}

struct Device {
    serial: String,
    state: String,
}

fn list_devices(adb: &Path) -> Vec<Device> {
    let mut out = Vec::new();
    let text = match run_adb(adb, &["devices"]) {
        Some(t) => t,
        None => return out,
    };

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('*') || line.starts_with("List of") {
            continue;
        }
        let mut parts = line.split_whitespace();
        let serial = match parts.next() {
            Some(s) => s,
            None => continue,
        };
        let state = match parts.next() {
            Some(s) => s.to_ascii_lowercase(),
            None => continue,
        };
        if !DEVICE_STATES.contains(&state.as_str()) {
            continue;
        }
        out.push(Device {
            serial: serial.to_string(),
            state,
        });
    }
    out
}

fn ready_device(adb: &Path) -> Option<String> {
    list_devices(adb)
        .into_iter()
        .find(|d| d.state == "device")
        .map(|d| d.serial)
}

/// adb 34 refuses bare mDNS instance names ("Invalid port while parsing address"),
/// so resolve the advertised service to `ip:port` before talking to adb.
fn mdns_addr(adb: &Path, service_type: &str, prefer_instance: &str) -> Option<String> {
    let text = run_adb(adb, &["mdns", "services"])?;
    let mut fallback = None;
    for line in text.lines() {
        if !line.contains(service_type) {
            continue;
        }
        let mut parts = line.split_whitespace();
        let instance = parts.next().unwrap_or("");
        let addr = match parts.find(|t| looks_like_addr(t)) {
            Some(a) => a,
            None => continue,
        };
        if !prefer_instance.is_empty() && instance == prefer_instance {
            return Some(addr.to_string());
        }
        if fallback.is_none() {
            fallback = Some(addr.to_string());
        }
    }
    fallback
}

fn looks_like_addr(token: &str) -> bool {
    match token.rsplit_once(':') {
        Some((host, port)) => {
            !host.is_empty()
                && !port.is_empty()
                && port.chars().all(|c| c.is_ascii_digit())
                && token.matches(':').count() <= 2
        }
        None => false,
    }
}

// ---------------------------------------------------------------- QR window

fn random_code() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut state = nanos ^ ((std::process::id() as u64) << 32) ^ 0x9E37_79B9_7F4A_7C15 | 1;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..8 {
        next();
    }
    format!("{:06}", next() % 1_000_000)
}

fn qr_bitmap(content: &str) -> Option<(usize, Vec<u32>)> {
    let code = QrCode::new(content.as_bytes()).ok()?;
    let modules = code.width();
    let colors = code.to_colors();
    if colors.len() != modules * modules {
        return None;
    }

    let dim = (modules + QUIET_MODULES * 2) * MODULE_PX;
    let mut buf = vec![0x00FF_FFFFu32; dim * dim];
    for y in 0..modules {
        for x in 0..modules {
            if colors[y * modules + x] != QrColor::Dark {
                continue;
            }
            let x0 = (x + QUIET_MODULES) * MODULE_PX;
            let y0 = (y + QUIET_MODULES) * MODULE_PX;
            for py in y0..y0 + MODULE_PX {
                let row = py * dim;
                for px in x0..x0 + MODULE_PX {
                    buf[row + px] = 0x0000_0000;
                }
            }
        }
    }
    Some((dim, buf))
}

fn run_pairing(adb: &Path) -> Option<String> {
    let code = random_code();
    eprintln!("[INFO] pairing code: {code}");
    eprintln!();
    eprintln!("  On the phone:");
    eprintln!("    Settings -> Developer options -> Wireless debugging");
    eprintln!("    -> \"Pair device with QR code\"");
    eprintln!("  Then point the phone camera at the QR code in the window.");
    eprintln!("  Press Esc or close the window to give up.");
    eprintln!();

    let content = format!("WIFI:T:ADB;S:{SERVICE_NAME};P:{code};;");
    let (dim, buf) = match qr_bitmap(&content) {
        Some(v) => v,
        None => {
            eprintln!("[ERROR] could not build the QR code");
            return None;
        }
    };

    let mut window = match Window::new(
        TITLE,
        dim,
        dim,
        WindowOptions {
            resize: false,
            scale: Scale::X1,
            ..Default::default()
        },
    ) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("[ERROR] cannot open window: {e}");
            return None;
        }
    };
    window.set_target_fps(30);

    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    {
        let adb = adb.to_path_buf();
        let code = code.clone();
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            let result = pair_loop(&adb, &code, &stop);
            let _ = tx.send(result);
        });
    }

    let mut result = None;
    loop {
        match rx.try_recv() {
            Ok(v) => {
                result = v;
                break;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => break,
        }
        if !window.is_open() {
            eprintln!("[INFO] window closed, giving up");
            break;
        }
        if window.is_key_pressed(Key::Escape, KeyRepeat::No) || window.is_key_down(Key::Escape) {
            eprintln!("[INFO] Escape pressed, giving up");
            break;
        }
        if window.update_with_buffer(&buf, dim, dim).is_err() {
            eprintln!("[INFO] window closed, giving up");
            break;
        }
    }
    stop.store(true, Ordering::SeqCst);
    result
}

// ------------------------------------------------------------ pairing logic

fn pair_loop(adb: &Path, code: &str, stop: &AtomicBool) -> Option<String> {
    let deadline = Instant::now() + PAIR_TIMEOUT;
    let mut attempt = 0u32;
    let mut last_out = String::new();

    while attempt < PAIR_ATTEMPTS && Instant::now() < deadline {
        if stop.load(Ordering::SeqCst) {
            return None;
        }
        attempt += 1;

        let host = mdns_addr(adb, PAIR_SERVICE, SERVICE_NAME)
            .unwrap_or_else(|| SERVICE_NAME.to_string());
        eprintln!("[PAIR] attempt {attempt}/{PAIR_ATTEMPTS}: adb pair {host} {code}");

        match run_adb_interruptible(
            adb,
            &["pair", &host, code],
            stop,
            PAIR_ATTEMPT_TIMEOUT,
        ) {
            Some(out) => {
                // before the phone shows its pairing dialog every attempt fails
                // with the same message; print it once instead of 60 times
                if out.trim() != last_out.trim() {
                    for line in out.lines() {
                        if !line.trim().is_empty() {
                            eprintln!("   {line}");
                        }
                    }
                    last_out = out.clone();
                }
                if out.contains("Successfully paired") {
                    eprintln!("[PAIR] paired");
                    match connect_endpoint(adb, &out, stop) {
                        Some(addr) => {
                            if let Some(target) = connect(adb, &addr, stop) {
                                return Some(target);
                            }
                            eprintln!("[PAIR] could not verify the connection, retrying");
                        }
                        None => eprintln!("[PAIR] no connect endpoint found, retrying"),
                    }
                }
            }
            None => {
                if stop.load(Ordering::SeqCst) {
                    return None;
                }
                eprintln!("[PAIR] adb pair gave no answer, retrying");
            }
        }

        if !sleep_interruptible(PAIR_INTERVAL, stop) {
            return None;
        }
    }

    eprintln!("[PAIR] timed out after {attempt} attempt(s)");
    None
}

/// After pairing the phone advertises `_adb-tls-connect._tcp`; prefer that
/// address, fall back to the guid printed by `adb pair`.
fn connect_endpoint(adb: &Path, pair_out: &str, stop: &AtomicBool) -> Option<String> {
    let deadline = Instant::now() + CONNECT_DISCOVERY;
    loop {
        if let Some(addr) = mdns_addr(adb, CONNECT_SERVICE, "") {
            return Some(addr);
        }
        if Instant::now() >= deadline || stop.load(Ordering::SeqCst) {
            break;
        }
        if !sleep_interruptible(Duration::from_secs(1), stop) {
            return None;
        }
    }
    parse_guid(pair_out)
}

fn parse_guid(out: &str) -> Option<String> {
    let start = out.find("guid=")? + "guid=".len();
    let rest = &out[start..];
    let end = rest
        .find(|c: char| c == ']' || c == ',' || c.is_whitespace())
        .unwrap_or(rest.len());
    let guid = rest[..end].trim();
    if guid.is_empty() {
        None
    } else {
        Some(guid.to_string())
    }
}

fn connect(adb: &Path, addr: &str, stop: &AtomicBool) -> Option<String> {
    let before: HashSet<String> = list_devices(adb).into_iter().map(|d| d.serial).collect();
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let mut next_connect = Instant::now();

    loop {
        if stop.load(Ordering::SeqCst) {
            return None;
        }

        if Instant::now() >= next_connect {
            eprintln!("[CONN] adb connect {addr}");
            if let Some(out) = run_adb(adb, &["connect", addr]) {
                for line in out.lines() {
                    if !line.trim().is_empty() {
                        eprintln!("   {line}");
                    }
                }
            }
            next_connect = Instant::now() + CONNECT_RETRY;
        }

        let devices = list_devices(adb);
        if let Some(serial) = pick_connected(&devices, addr, &before) {
            eprintln!("[CONN] device ready: {serial}");
            return Some(serial);
        }
        if Instant::now() >= deadline {
            eprintln!("[CONN] device did not show up as 'device' within {CONNECT_TIMEOUT:?}");
            return None;
        }
        if !sleep_interruptible(CONNECT_INTERVAL, stop) {
            return None;
        }
    }
}

fn pick_connected(devices: &[Device], addr: &str, before: &HashSet<String>) -> Option<String> {
    let ready = devices.iter().filter(|d| d.state == "device");

    for d in ready.clone() {
        if serial_matches(&d.serial, addr) {
            return Some(d.serial.clone());
        }
    }

    // adb may register the device under a different name than the address we
    // connected to, so accept any freshly appeared device as well.
    for d in ready {
        if !before.contains(&d.serial) {
            return Some(d.serial.clone());
        }
    }
    None
}

fn serial_matches(serial: &str, addr: &str) -> bool {
    let s = serial.to_ascii_lowercase();
    let a = addr.to_ascii_lowercase();
    s == a || s.starts_with(&a) || a.starts_with(&s)
}

fn sleep_interruptible(duration: Duration, stop: &AtomicBool) -> bool {
    let end = Instant::now() + duration;
    while Instant::now() < end {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        thread::sleep(Duration::from_millis(100));
    }
    !stop.load(Ordering::SeqCst)
}
