//! hnubl - Honor 8E5 (BKQ-AN90) / AN00 (BRM-AN00) one-click root / KernelSU / OTA tool, single-file GUI.
//!
//! Replaces main.bat + reroot.bat + adb_connect.bat + adbpair.exe:
//!   * finds adb next to the exe, uses an already connected device when there is one
//!   * otherwise shows a QR code for wireless adb pairing and polls `adb pair` in a
//!     background thread until the phone scans it
//!   * menu: get root (CVE-2026-98260 + KernelSU), disable / enable the OTA updater
//!   * root backend selected by kernel: 8E5 uses magic-98260 + reroot.sh,
//!     AN00 uses brm-an00 + reroot-an00.sh with KernelSU 3.3.1 late-load
//!   * every adb command streams its output into an egui log view
//!
//! All adb children are spawned with `ADB_MDNS_OPENSCREEN=1` so mDNS discovery works
//! on Windows without the Bonjour daemon.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::HashSet;
use std::env;
use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui;
use qrcode::Color as QrColor;
use qrcode::QrCode;
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

const APP_TITLE: &str = "荣耀 8E5 / 8E6 一键 Root 工具";

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

const INSTALL_TIMEOUT: Duration = Duration::from_secs(240);
const PUSH_TIMEOUT: Duration = Duration::from_secs(600);
const EXPLOIT_TIMEOUT: Duration = Duration::from_secs(900);
const OTA_TIMEOUT: Duration = Duration::from_secs(180);

const MAX_LOG_LINES: usize = 6000;

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
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 800.0])
            .with_min_inner_size([720.0, 560.0])
            .with_title(APP_TITLE),
        renderer: eframe::Renderer::Glow,
        centered: true,
        ..Default::default()
    };

    if let Err(err) = eframe::run_native(
        "hnubl",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    ) {
        fatal(&format!("无法启动图形界面：{err}"));
        std::process::exit(1);
    }
}

// ---------------------------------------------------------------- fatal error

#[cfg(windows)]
fn fatal(message: &str) {
    use std::os::windows::ffi::OsStrExt as _;

    let text: Vec<u16> = std::ffi::OsStr::new(message)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let title: Vec<u16> = "hnubl".encode_utf16().chain(std::iter::once(0)).collect();
    // MB_ICONERROR | MB_OK
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            0x0000_0010,
        );
    }
}

#[cfg(not(windows))]
fn fatal(message: &str) {
    eprintln!("{message}");
}

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    fn MessageBoxW(
        hwnd: *mut std::ffi::c_void,
        text: *const u16,
        caption: *const u16,
        utype: u32,
    ) -> i32;
}

// ---------------------------------------------------------------- paths / adb

/// Directory that holds `hnubl.exe`. Tool files live in subdirectories:
/// `tools/adb.exe` and `assets/{ksu.apk, reroot.sh, magic-98260}`.
///
/// Normally the directory of the exe. While developing (`target/release/hnubl.exe`)
/// walk up a few levels so `cargo run` still finds the toolkit files.
fn base_dir() -> PathBuf {
    let exe = env::current_exe().unwrap_or_else(|_| PathBuf::from("."));
    let start = exe.parent().unwrap_or(Path::new(".")).to_path_buf();
    for dir in start.ancestors().take(4) {
        if dir.join("assets").is_dir() || dir.join("tools").is_dir() {
            return dir.to_path_buf();
        }
    }
    start
}

fn find_adb(base: &Path) -> Option<PathBuf> {
    // prefer the bundled tools/ directory next to the exe
    let tools = base.join("tools").join("adb.exe");
    if tools.is_file() {
        return Some(tools);
    }
    let beside = base.join("adb.exe");
    if beside.is_file() {
        return Some(beside);
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

#[derive(Clone)]
struct Adb {
    path: PathBuf,
    base: PathBuf,
    serial: Option<String>,
}

impl Adb {
    /// `-s <serial>` goes before the adb sub-command.
    fn full<'a>(&'a self, rest: &[&'a str]) -> Vec<&'a str> {
        match &self.serial {
            Some(serial) if !rest.is_empty() => {
                let mut args = Vec::with_capacity(rest.len() + 2);
                args.push("-s");
                args.push(serial.as_str());
                args.extend_from_slice(rest);
                args
            }
            _ => rest.to_vec(),
        }
    }

    fn command(&self, rest: &[&str]) -> Command {
        let args = self.full(rest);
        let mut cmd = Command::new(&self.path);
        cmd.env("ADB_MDNS_OPENSCREEN", "1")
            .current_dir(&self.base)
            .args(&args)
            .stdin(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW);
        cmd
    }

    /// Run without `-s` (device discovery must see every device).
    fn run_raw(&self, rest: &[&str]) -> Option<String> {
        let mut cmd = Command::new(&self.path);
        cmd.env("ADB_MDNS_OPENSCREEN", "1")
            .current_dir(&self.base)
            .args(rest)
            .stdin(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW);
        let out = cmd.output().ok()?;
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        Some(text)
    }

    fn run(&self, rest: &[&str]) -> Option<String> {
        let out = self.command(rest).output().ok()?;
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        Some(text)
    }

    /// Same as [`Adb::run_raw`], but pollable so a closed window aborts the child early.
    fn run_interruptible(&self, rest: &[&str], stop: &AtomicBool, limit: Duration) -> Option<String> {
        let mut child = Command::new(&self.path)
            .env("ADB_MDNS_OPENSCREEN", "1")
            .current_dir(&self.base)
            .args(rest)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
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

    /// Run adb and stream every output line to the GUI as it arrives.
    fn stream(
        &self,
        rest: &[&str],
        stop: &AtomicBool,
        tx: &mpsc::Sender<Event>,
        limit: Duration,
    ) -> Result<String, String> {
        let mut child = self
            .command(rest)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("无法启动 adb：{e}"))?;

        let mut pumps = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            pumps.push(pump(stdout, tx.clone()));
        }
        if let Some(stderr) = child.stderr.take() {
            pumps.push(pump(stderr, tx.clone()));
        }

        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("等待 adb 结束失败：{e}"));
                }
            }
            if stop.load(Ordering::SeqCst) {
                let _ = child.kill();
                let _ = child.wait();
                for pump in pumps {
                    let _ = pump.join();
                }
                return Err("已取消".to_string());
            }
            if started.elapsed() > limit {
                let _ = child.kill();
                let _ = child.wait();
                for pump in pumps {
                    let _ = pump.join();
                }
                return Err(format!("超时（{} 秒）", limit.as_secs()));
            }
            thread::sleep(Duration::from_millis(80));
        };

        let mut text = String::new();
        for pump in pumps {
            if let Ok(chunk) = pump.join() {
                text.push_str(&chunk);
            }
        }

        if status.success() {
            Ok(text)
        } else {
            Err(format!("adb {} 失败（{status}）", rest.join(" ")))
        }
    }
}

/// Read a pipe line by line, forwarding each line to the GUI and collecting it.
fn pump<R: Read + Send + 'static>(
    mut reader: R,
    tx: mpsc::Sender<Event>,
) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut collected = String::new();
        let mut pending: Vec<u8> = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            pending.extend_from_slice(&buf[..n]);
            while let Some(pos) = pending.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = pending.drain(..=pos).collect();
                emit_line(&tx, &line, &mut collected);
            }
        }
        if !pending.is_empty() {
            emit_line(&tx, &pending, &mut collected);
        }
        collected
    })
}

fn emit_line(tx: &mpsc::Sender<Event>, raw: &[u8], collected: &mut String) {
    let text = String::from_utf8_lossy(raw);
    let line = text.trim_end_matches(['\r', '\n']);
    if line.is_empty() {
        return;
    }
    collected.push_str(line);
    collected.push('\n');
    let _ = tx.send(Event::Log(line.to_string()));
}

// ------------------------------------------------------------- device lookup

struct Device {
    serial: String,
    state: String,
}

fn list_devices(adb: &Adb) -> Vec<Device> {
    let mut out = Vec::new();
    let text = match adb.run_raw(&["devices"]) {
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

fn ready_device(adb: &Adb) -> Option<String> {
    list_devices(adb)
        .into_iter()
        .find(|d| d.state == "device")
        .map(|d| d.serial)
}

/// adb 34 refuses bare mDNS instance names ("Invalid port while parsing address"),
/// so resolve the advertised service to `ip:port` before talking to adb.
fn mdns_addr(adb: &Adb, service_type: &str, prefer_instance: &str) -> Option<String> {
    let text = adb.run_raw(&["mdns", "services"])?;
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

// ---------------------------------------------------------------- QR code

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

/// `(side, 0x00RRGGBB pixels, row major)` of the QR code, quiet zone included.
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

fn qr_color_image(content: &str) -> Option<egui::ColorImage> {
    let (dim, pixels) = qr_bitmap(content)?;
    let mut rgb = Vec::with_capacity(dim * dim * 3);
    for px in pixels {
        rgb.push((px >> 16) as u8);
        rgb.push((px >> 8) as u8);
        rgb.push(px as u8);
    }
    Some(egui::ColorImage::from_rgb([dim, dim], &rgb))
}

// ------------------------------------------------------------ pairing logic

/// Blocking pairing worker; every step is logged to the GUI.
fn pair_loop(adb: &Adb, code: &str, stop: &AtomicBool, tx: &mpsc::Sender<Event>) -> Option<String> {
    let deadline = Instant::now() + PAIR_TIMEOUT;
    let mut attempt = 0u32;
    let mut last_out = String::new();

    while attempt < PAIR_ATTEMPTS && Instant::now() < deadline {
        if stop.load(Ordering::SeqCst) {
            return None;
        }
        attempt += 1;

        let host =
            mdns_addr(adb, PAIR_SERVICE, SERVICE_NAME).unwrap_or_else(|| SERVICE_NAME.to_string());
        let _ = tx.send(Event::Status(format!(
            "第 {attempt}/{PAIR_ATTEMPTS} 次尝试配对（{host}）…"
        )));
        log(tx, &format!("[配对] 第 {attempt} 次：adb pair {host} {code}"));

        match adb.run_interruptible(&["pair", &host, code], stop, PAIR_ATTEMPT_TIMEOUT) {
            Some(out) => {
                // before the phone shows its pairing dialog every attempt fails
                // with the same message; print it once instead of 60 times
                if out.trim() != last_out.trim() {
                    for line in out.lines() {
                        if !line.trim().is_empty() {
                            log(tx, &format!("   {line}"));
                        }
                    }
                    last_out = out.clone();
                }
                if out.contains("Successfully paired") {
                    log(tx, "[配对] 配对成功，正在建立连接…");
                    let _ = tx.send(Event::Status("配对成功，正在连接设备…".to_string()));
                    match connect_endpoint(adb, &out, stop) {
                        Some(addr) => {
                            if let Some(target) = connect(adb, &addr, stop, tx) {
                                return Some(target);
                            }
                            log(tx, "[配对] 连接未确认，重试");
                        }
                        None => log(tx, "[配对] 未找到连接端点，重试"),
                    }
                }
            }
            None => {
                if stop.load(Ordering::SeqCst) {
                    return None;
                }
                log(tx, "[配对] adb pair 没有响应，重试");
            }
        }

        if !sleep_interruptible(PAIR_INTERVAL, stop) {
            return None;
        }
    }

    log(tx, &format!("[配对] 超时，共 {attempt} 次尝试"));
    None
}

/// After pairing the phone advertises `_adb-tls-connect._tcp`; prefer that
/// address, fall back to the guid printed by `adb pair`.
fn connect_endpoint(adb: &Adb, pair_out: &str, stop: &AtomicBool) -> Option<String> {
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

fn connect(adb: &Adb, addr: &str, stop: &AtomicBool, tx: &mpsc::Sender<Event>) -> Option<String> {
    let before: HashSet<String> = list_devices(adb).into_iter().map(|d| d.serial).collect();
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let mut next_connect = Instant::now();

    loop {
        if stop.load(Ordering::SeqCst) {
            return None;
        }

        if Instant::now() >= next_connect {
            log(tx, &format!("[连接] adb connect {addr}"));
            if let Some(out) = adb.run_raw(&["connect", addr]) {
                for line in out.lines() {
                    if !line.trim().is_empty() {
                        log(tx, &format!("   {line}"));
                    }
                }
            }
            next_connect = Instant::now() + CONNECT_RETRY;
        }

        let devices = list_devices(adb);
        if let Some(serial) = pick_connected(&devices, addr, &before) {
            log(tx, &format!("[连接] 设备就绪：{serial}"));
            return Some(serial);
        }
        if Instant::now() >= deadline {
            log(tx, "[连接] 设备未在限定时间内就绪");
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

// ------------------------------------------------------------------ jobs

#[derive(Clone, Copy, PartialEq, Eq)]
enum JobKind {
    Root,
    DisableOta,
    EnableOta,
}

impl JobKind {
    fn title(self) -> &'static str {
        match self {
            JobKind::Root => "获取 Root（98260 提权 + KernelSU，自动识别 8E5 / 8E6）",
            JobKind::DisableOta => "禁用 OTA 更新器",
            JobKind::EnableOta => "启用 OTA 更新器",
        }
    }
}

fn log(tx: &mpsc::Sender<Event>, line: &str) {
    let _ = tx.send(Event::Log(line.to_string()));
}

fn require_file(path: &Path, what: &str) -> Result<(), String> {
    if path.exists() {
        Ok(())
    } else {
        Err(format!("缺少{what}：{}", path.display()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Profile {
    E8E5,
    An00,
    Unsupported,
}

fn detect_profile(adb: &Adb) -> Profile {
    let uname = adb.run(&["shell", "uname -r"]).unwrap_or_default();
    let model = adb
        .run(&["shell", "getprop ro.product.model"])
        .unwrap_or_default();
    log_uname_model(&uname, &model);
    if uname.contains("6.18.21-android17") {
        return Profile::An00;
    }
    if uname.contains("6.12.58-android16") || model.contains("BKQ") {
        return Profile::E8E5;
    }
    Profile::Unsupported
}

fn log_uname_model(uname: &str, model: &str) {
    let mut lines = String::new();
    for (label, value) in [("uname", uname), ("model", model)] {
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        if !lines.is_empty() {
            lines.push('\n');
        }
        for line in value.lines() {
            lines.push_str(label);
            lines.push_str(": ");
            lines.push_str(line);
            lines.push('\n');
        }
    }
    if !lines.is_empty() {
        eprint!("{lines}");
    }
}

fn describe_profile(profile: Profile) -> &'static str {
    match profile {
        Profile::E8E5 => "8E5",
        Profile::An00 => "AN00",
        Profile::Unsupported => "未知",
    }
}

fn job_root(
    adb: &Adb,
    stop: &AtomicBool,
    tx: &mpsc::Sender<Event>,
) -> Result<(), String> {
    let profile = detect_profile(adb);
    log(
        tx,
        &format!("== 检测机型：{} ==", describe_profile(profile)),
    );
    match profile {
        Profile::E8E5 => job_root_e8e5(adb, stop, tx),
        Profile::An00 => job_root_an00(adb, stop, tx),
        Profile::Unsupported => {
            let uname = adb.run(&["shell", "uname -r"]).unwrap_or_default();
            Err(format!("不支持的内核/机型：`{}`", uname.trim()))
        }
    }
}

fn job_root_e8e5(
    adb: &Adb,
    stop: &AtomicBool,
    tx: &mpsc::Sender<Event>,
) -> Result<(), String> {
    log(tx, "== 8E5 [1/6] 检查设备 ==");
    let devices = list_devices(adb);
    let device = devices
        .iter()
        .find(|d| d.state == "device")
        .ok_or_else(|| "没有处于 device 状态的设备，请重新连接".to_string())?;
    log(tx, &format!("已连接设备：{}", device.serial));
    if let Some(out) = adb.run(&["shell", "echo Device: $(getprop ro.product.model)"]) {
        for line in out.lines().filter(|l| !l.trim().is_empty()) {
            log(tx, &format!("   {}", line.trim()));
        }
    }

    log(tx, "== 8E5 [2/6] 检查 KernelSU 管理器 ==");
    let pm_path = adb
        .run(&["shell", "pm path me.weishu.kernelsu"])
        .unwrap_or_default();
    if pm_path.contains("base.apk") {
        log(tx, "   管理器已安装，跳过");
    } else {
        let apk = adb.base.join("assets").join("ksu.apk");
        require_file(&apk, "KernelSU 安装包")?;
        let apk = apk.to_string_lossy().into_owned();
        log(tx, "   未安装，正在安装 KernelSU v3.3.1 …");
        adb.stream(&["install", "-r", &apk], stop, tx, INSTALL_TIMEOUT)?;
        log(tx, "   管理器安装完成");
    }

    log(tx, "== 8E5 [3/6] 推送提权资产 ==");
    let assets = adb.base.join("assets").join("magic-98260");
    require_file(&assets, "提权资产目录")?;
    let assets = assets.to_string_lossy().into_owned();
    adb.run(&["shell", "rm -rf /data/local/tmp/magic-98260"]);
    adb.stream(&["push", &assets, "/data/local/tmp/"], stop, tx, PUSH_TIMEOUT)?;
    adb.stream(
        &["shell", "chmod -R 755 /data/local/tmp/magic-98260"],
        stop,
        tx,
        PUSH_TIMEOUT,
    )?;

    log(tx, "== 8E5 [4/6] 推送 reroot.sh ==");
    let script = adb.base.join("assets").join("reroot.sh");
    require_file(&script, "reroot.sh")?;
    let script = script.to_string_lossy().into_owned();
    adb.stream(
        &["push", &script, "/data/local/tmp/reroot.sh"],
        stop,
        tx,
        PUSH_TIMEOUT,
    )?;
    adb.stream(
        &["shell", "chmod 755 /data/local/tmp/reroot.sh"],
        stop,
        tx,
        PUSH_TIMEOUT,
    )?;

    log(tx, "== 8E5 [5/6] 执行 98260 提权链 + 加载 KernelSU 模块（约 1~5 分钟）==");
    adb.stream(
        &["shell", "sh /data/local/tmp/reroot.sh"],
        stop,
        tx,
        EXPLOIT_TIMEOUT,
    )?;

    log(tx, "== 8E5 [6/6] 完成，请在手机上打开 KernelSU 管理器确认状态 ==");
    Ok(())
}

fn job_root_an00(
    adb: &Adb,
    stop: &AtomicBool,
    tx: &mpsc::Sender<Event>,
) -> Result<(), String> {
    log(tx, "== AN00 [1/5] 推送 exploit ==");
    let assets = adb.base.join("assets").join("brm-an00");
    for name in [
        "uaf_plant_replay",
        "slide_probe",
        "pc98260-su",
        "cap_check",
    ] {
        if !assets.join(name).is_file() {
            return Err(format!(
                "缺少 {name}；请先跑 GitHub Actions 下载 exploit 放进 assets/brm-an00/"
            ));
        }
    }
    let ksud = adb.base.join("assets").join("magic-98260").join("ksud");
    require_file(&ksud, "KernelSU ksud")?;
    let apk = adb.base.join("assets").join("ksu.apk");
    require_file(&apk, "KernelSU 安装包")?;
    let script = adb.base.join("assets").join("reroot-an00.sh");
    require_file(&script, "reroot-an00.sh")?;

    let assets_path = assets.to_string_lossy().into_owned();
    adb.run(&["shell", "rm -rf /data/local/tmp/brm-an00"]);
    adb.stream(
        &["push", &assets_path, "/data/local/tmp/brm-an00"],
        stop,
        tx,
        PUSH_TIMEOUT,
    )?;
    adb.stream(
        &["shell", "chmod -R 755 /data/local/tmp/brm-an00"],
        stop,
        tx,
        PUSH_TIMEOUT,
    )?;

    log(tx, "== AN00 [2/5] 推送 KernelSU 资产 ==");
    let ksud = ksud.to_string_lossy().into_owned();
    adb.stream(
        &["push", &ksud, "/data/local/tmp/brm-an00/ksud"],
        stop,
        tx,
        PUSH_TIMEOUT,
    )?;
    let apk = apk.to_string_lossy().into_owned();
    adb.stream(
        &["push", &apk, "/data/local/tmp/brm-an00/ksu.apk"],
        stop,
        tx,
        PUSH_TIMEOUT,
    )?;
    adb.stream(
        &["shell", "chmod 755 /data/local/tmp/brm-an00/ksud"],
        stop,
        tx,
        PUSH_TIMEOUT,
    )?;

    log(tx, "== AN00 [3/5] 检查 KernelSU 管理器 ==");
    let pm_path = adb
        .run(&["shell", "pm path me.weishu.kernelsu"])
        .unwrap_or_default();
    if pm_path.contains("base.apk") {
        log(tx, "   管理器已安装，跳过");
    } else {
        log(tx, "   未安装，正在安装 KernelSU v3.3.1 …");
        adb.stream(&["install", "-r", &apk], stop, tx, INSTALL_TIMEOUT)?;
        log(tx, "   管理器安装完成");
    }

    log(tx, "== AN00 [4/5] 推送 reroot-an00.sh ==");
    let script = script.to_string_lossy().into_owned();
    adb.stream(
        &["push", &script, "/data/local/tmp/reroot-an00.sh"],
        stop,
        tx,
        PUSH_TIMEOUT,
    )?;
    adb.stream(
        &["shell", "chmod 755 /data/local/tmp/reroot-an00.sh"],
        stop,
        tx,
        PUSH_TIMEOUT,
    )?;

    log(tx, "== AN00 [5/5] 执行 98260 exploit + KernelSU late-load（约 1~5 分钟）==");
    adb.stream(
        &["shell", "sh /data/local/tmp/reroot-an00.sh"],
        stop,
        tx,
        EXPLOIT_TIMEOUT,
    )?;
    log(tx, "== AN00 完成，请在手机上打开 KernelSU 管理器确认状态 ==");
    Ok(())
}

fn job_disable_ota(adb: &Adb, stop: &AtomicBool, tx: &mpsc::Sender<Event>) -> Result<(), String> {
    log(tx, "== 禁用 OTA 更新器（com.hihonor.ouc）==");
    adb.stream(
        &[
            "shell",
            "su -c 'pm uninstall -k --user 0 com.hihonor.ouc'",
        ],
        stop,
        tx,
        OTA_TIMEOUT,
    )?;
    log(tx, "== 完成 ==");
    Ok(())
}

fn job_enable_ota(adb: &Adb, stop: &AtomicBool, tx: &mpsc::Sender<Event>) -> Result<(), String> {
    log(tx, "== 启用 OTA 更新器（com.hihonor.ouc）==");
    adb.stream(
        &[
            "shell",
            "su -c 'cmd package install-existing com.hihonor.ouc'",
        ],
        stop,
        tx,
        OTA_TIMEOUT,
    )?;
    log(tx, "== 完成 ==");
    Ok(())
}

fn run_job(adb: Adb, kind: JobKind, stop: Arc<AtomicBool>, tx: mpsc::Sender<Event>) {
    let result = match kind {
        JobKind::Root => job_root(&adb, &stop, &tx),
        JobKind::DisableOta => job_disable_ota(&adb, &stop, &tx),
        JobKind::EnableOta => job_enable_ota(&adb, &stop, &tx),
    };

    let (ok, note) = match result {
        Ok(()) => {
            log(&tx, "");
            log(&tx, "[完成] 全部步骤执行成功");
            (true, "执行完成".to_string())
        }
        Err(err) => {
            log(&tx, "");
            log(&tx, &format!("[失败] {err}"));
            (false, err)
        }
    };
    let _ = tx.send(Event::JobDone { ok, note });
}

/// Root state (`su -c id` -> uid=0) and OTA updater presence.
fn probe_device(adb: &Adb) -> (Option<bool>, Option<bool>) {
    let root = adb
        .run(&["shell", "su -c id"])
        .map(|out| out.contains("uid=0"));
    let ouc = adb
        .run(&["shell", "pm path com.hihonor.ouc"])
        .map(|out| out.contains("base.apk"));
    (root, ouc)
}

// -------------------------------------------------------------- GUI plumbing

enum Event {
    Log(String),
    Status(String),
    Paired(Option<String>),
    Probe { root: Option<bool>, ouc: Option<bool> },
    JobDone { ok: bool, note: String },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Pairing,
    Menu,
    Running,
}

struct App {
    adb: Option<Adb>,
    tx: mpsc::Sender<Event>,
    rx: mpsc::Receiver<Event>,

    page: Page,
    qr_content: String,
    qr_texture: Option<egui::TextureHandle>,
    pairing_active: bool,

    serial: Option<String>,
    root_ok: Option<bool>,
    ouc_ok: Option<bool>,

    logs: Vec<String>,
    status: String,

    job: Option<JobKind>,
    job_running: bool,
    job_ok: Option<bool>,
    job_note: String,

    stop: Arc<AtomicBool>,
    shutdown_done: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_cjk_font(&cc.egui_ctx);
        cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
        cc.egui_ctx.all_styles_mut(|style| {
            style.visuals = egui::Visuals::dark();
            style.spacing.item_spacing = egui::vec2(8.0, 10.0);
            style.text_styles.insert(
                egui::TextStyle::Body,
                egui::FontId::new(16.0, egui::FontFamily::Proportional),
            );
            style.text_styles.insert(
                egui::TextStyle::Button,
                egui::FontId::new(17.0, egui::FontFamily::Proportional),
            );
            style.text_styles.insert(
                egui::TextStyle::Heading,
                egui::FontId::new(23.0, egui::FontFamily::Proportional),
            );
            style.text_styles.insert(
                egui::TextStyle::Monospace,
                egui::FontId::new(13.0, egui::FontFamily::Monospace),
            );
        });

        let (tx, rx) = mpsc::channel();
        let base = base_dir();
        let adb = find_adb(&base).map(|path| Adb {
            path,
            base,
            serial: None,
        });

        let mut app = Self {
            adb,
            tx,
            rx,
            page: Page::Pairing,
            qr_content: String::new(),
            qr_texture: None,
            pairing_active: false,
            serial: None,
            root_ok: None,
            ouc_ok: None,
            logs: Vec::new(),
            status: String::new(),
            job: None,
            job_running: false,
            job_ok: None,
            job_note: String::new(),
            stop: Arc::new(AtomicBool::new(false)),
            shutdown_done: false,
        };

        match app.adb {
            Some(_) => app.begin_pairing(),
            None => {
                app.status = "没有找到 tools/adb.exe：请保持 hnubl.exe 与 tools/、assets/ 目录在一起。".to_string();
            }
        }
        app
    }

    /// Build a fresh pairing code/QR and start the pairing worker thread.
    fn begin_pairing(&mut self) {
        let adb = match self.adb.clone() {
            Some(adb) => adb,
            None => return,
        };

        self.stop.store(true, Ordering::SeqCst);
        self.page = Page::Pairing;
        self.serial = None;
        self.root_ok = None;
        self.ouc_ok = None;
        self.pairing_active = true;
        self.qr_texture = None;
        self.status = "正在搜索设备…".to_string();

        let code = random_code();
        self.qr_content = format!("WIFI:T:ADB;S:{SERVICE_NAME};P:{code};;");

        let stop = Arc::new(AtomicBool::new(false));
        self.stop = Arc::clone(&stop);
        let tx = self.tx.clone();
        log(
            &tx,
            &format!("[配对] 二维码内容已生成，配对码 {code}"),
        );

        thread::spawn(move || {
            if let Some(serial) = ready_device(&adb) {
                log(&tx, &format!("[配对] 已有设备处于 device 状态：{serial}"));
                let _ = tx.send(Event::Paired(Some(serial)));
                return;
            }
            log(&tx, "[配对] 没有已连接设备，开始无线配对");
            let result = pair_loop(&adb, &code, &stop, &tx);
            let _ = tx.send(Event::Paired(result));
        });
    }

    fn enter_menu(&mut self, serial: String) {
        self.serial = Some(serial.clone());
        if let Some(adb) = self.adb.as_mut() {
            adb.serial = Some(serial);
        }
        self.page = Page::Menu;
        self.pairing_active = false;
        self.status.clear();
        self.start_probe();
    }

    fn start_probe(&mut self) {
        let adb = match self.adb.clone() {
            Some(adb) => adb,
            None => return,
        };
        self.root_ok = None;
        self.ouc_ok = None;
        let tx = self.tx.clone();
        thread::spawn(move || {
            let (root, ouc) = probe_device(&adb);
            let _ = tx.send(Event::Probe { root, ouc });
        });
    }

    fn start_job(&mut self, kind: JobKind) {
        let adb = match self.adb.clone() {
            Some(adb) => adb,
            None => return,
        };
        self.stop.store(true, Ordering::SeqCst);
        self.page = Page::Running;
        self.job = Some(kind);
        self.job_running = true;
        self.job_ok = None;
        self.job_note.clear();
        self.logs.clear();
        log(&self.tx, &format!("===== {} =====", kind.title()));

        let stop = Arc::new(AtomicBool::new(false));
        self.stop = Arc::clone(&stop);
        let tx = self.tx.clone();
        thread::spawn(move || run_job(adb, kind, stop, tx));
    }

    fn back_to_menu(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.job_running = false;
        self.page = Page::Menu;
        self.start_probe();
    }

    fn shutdown(&mut self) {
        if self.shutdown_done {
            return;
        }
        self.shutdown_done = true;
        self.stop.store(true, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(1000));
        if let Some(adb) = &self.adb {
            let _ = adb.run_raw(&["kill-server"]);
        }
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Log(line) => {
                    self.logs.push(line);
                    if self.logs.len() > MAX_LOG_LINES {
                        self.logs.drain(..MAX_LOG_LINES / 3);
                    }
                }
                Event::Status(text) => self.status = text,
                Event::Paired(Some(serial)) => self.enter_menu(serial),
                Event::Paired(None) => {
                    self.pairing_active = false;
                    if self.status.is_empty() || !self.status.contains("失败") {
                        self.status = "配对未完成：可点击“重试配对”，或检查手机是否已扫码。".to_string();
                    }
                }
                Event::Probe { root, ouc } => {
                    self.root_ok = root;
                    self.ouc_ok = ouc;
                }
                Event::JobDone { ok, note } => {
                    self.job_running = false;
                    self.job_ok = Some(ok);
                    self.job_note = note;
                }
            }
        }
    }

    fn qr_image(&mut self, ctx: &egui::Context) -> Option<egui::TextureHandle> {
        if self.qr_texture.is_none() {
            let image = qr_color_image(&self.qr_content)?;
            self.qr_texture =
                Some(ctx.load_texture("pair-qr", image, egui::TextureOptions::LINEAR));
        }
        self.qr_texture.clone()
    }

    fn pairing_page(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(6.0);
            ui.heading("手机扫码配对无线调试");
            ui.add_space(4.0);

            match self.qr_image(ui.ctx()) {
                Some(texture) => {
                    let side = ui.available_width().min(320.0).max(160.0);
                    let card = side + 24.0;
                    ui.allocate_ui_with_layout(
                        egui::vec2(card, card),
                        egui::Layout::top_down(egui::Align::Center),
                        |ui| {
                            egui::Frame::NONE
                                .fill(egui::Color32::WHITE)
                                .inner_margin(egui::Margin::same(12))
                                .show(ui, |ui| {
                                    ui.image((texture.id(), egui::vec2(side, side)));
                                });
                        },
                    );
                }
                None => {
                    ui.colored_label(egui::Color32::LIGHT_RED, "二维码生成失败");
                }
            }

            ui.add_space(8.0);
            ui.label(egui::RichText::new(
                "手机：设置 → 开发者选项 → 无线调试 → 使用二维码配对设备",
            ));
            ui.add_space(4.0);

            if self.pairing_active {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(16.0));
                    ui.label(if self.status.is_empty() {
                        "正在等待手机扫码…"
                    } else {
                        &self.status
                    });
                });
            } else {
                ui.colored_label(egui::Color32::from_rgb(255, 190, 90), &self.status);
            }

            ui.add_space(10.0);
            let row_width = 160.0 * 2.0 + ui.spacing().item_spacing.x;
            ui.allocate_ui_with_layout(
                egui::vec2(row_width, 38.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    if ui
                        .add_sized(
                            [160.0, 38.0],
                            egui::Button::new(if self.pairing_active {
                                "重试配对"
                            } else {
                                "重新配对"
                            }),
                        )
                        .clicked()
                    {
                        self.begin_pairing();
                    }
                    if ui
                        .add_sized([160.0, 38.0], egui::Button::new("取消并退出"))
                        .clicked()
                    {
                        self.shutdown();
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                },
            );
            });

        ui.add_space(6.0);
        self.log_area(ui);
    }

    fn menu_page(&mut self, ui: &mut egui::Ui) {
        let serial = self.serial.clone().unwrap_or_default();
        ui.horizontal(|ui| {
            ui.heading(APP_TITLE);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("退出").clicked() {
                    self.shutdown();
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        });
        ui.label(format!("当前设备：{serial}"));
        ui.separator();

        let state_text = |v: Option<bool>, yes: &str, no: &str| -> (String, egui::Color32) {
            match v {
                Some(true) => (yes.to_string(), egui::Color32::from_rgb(120, 220, 130)),
                Some(false) => (no.to_string(), egui::Color32::from_rgb(255, 150, 120)),
                None => ("检查中…".to_string(), egui::Color32::GRAY),
            }
        };

        let (root_text, root_color) = state_text(self.root_ok, "已获取", "未获取");
        let (ouc_text, ouc_color) = state_text(self.ouc_ok, "已启用", "已禁用");
        ui.horizontal(|ui| {
            ui.label("Root：");
            ui.colored_label(root_color, root_text);
            ui.add_space(20.0);
            ui.label("OTA 更新器（com.hihonor.ouc）：");
            ui.colored_label(ouc_color, ouc_text);
        });
        ui.add_space(10.0);

        let button = |ui: &mut egui::Ui, text: &str| -> bool {
            ui.add_sized(
                [420.0, 48.0],
                egui::Button::new(egui::RichText::new(text).size(18.0)),
            )
            .clicked()
        };

        ui.vertical_centered(|ui| {
            if button(ui, "获取 Root（98260 提权 + KernelSU，自动识别 8E5 / 8E6）") {
                self.start_job(JobKind::Root);
            }
            ui.add_space(6.0);
            if button(ui, "禁用 OTA 更新器") {
                self.start_job(JobKind::DisableOta);
            }
            ui.add_space(6.0);
            if button(ui, "启用 OTA 更新器") {
                self.start_job(JobKind::EnableOta);
            }
        });

        ui.add_space(10.0);
        self.log_area(ui);
    }

    fn running_page(&mut self, ui: &mut egui::Ui) {
        let title = self.job.map(|j| j.title()).unwrap_or("执行");
        ui.horizontal(|ui| {
            ui.heading(title);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("退出").clicked() {
                    self.shutdown();
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        });

        ui.horizontal(|ui| {
            if self.job_running {
                ui.add(egui::Spinner::new().size(16.0));
                ui.label("正在执行，请勿断开设备…");
            } else {
                match self.job_ok {
                    Some(true) => {
                        ui.colored_label(
                            egui::Color32::from_rgb(120, 220, 130),
                            format!("✔ {}", self.job_note),
                        );
                    }
                    Some(false) => {
                        ui.colored_label(
                            egui::Color32::from_rgb(255, 140, 120),
                            format!("✘ {}", self.job_note),
                        );
                    }
                    None => {}
                }
            }
        });
        ui.separator();

        let log_height = (ui.available_height() - 72.0).max(120.0);
        self.log_area_h(ui, log_height);

        ui.add_space(6.0);
        if ui
            .add_sized([180.0, 40.0], egui::Button::new("返回菜单"))
            .clicked()
        {
            self.back_to_menu();
        }
    }

    fn log_area(&mut self, ui: &mut egui::Ui) {
        let height = (ui.available_height() - 8.0).max(120.0);
        self.log_area_h(ui, height);
    }

    fn log_area_h(&mut self, ui: &mut egui::Ui, height: f32) {
        egui::Frame::NONE
            .fill(egui::Color32::from_rgb(14, 15, 18))
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .max_height(height)
                    .show(ui, |ui| {
                        ui.set_min_height(height);
                        for line in &self.logs {
                            ui.monospace(line);
                        }
                    });
            });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_events();
        ui.ctx().request_repaint_after(Duration::from_millis(120));

        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(egui::Color32::from_rgb(24, 26, 32))
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ui, |ui| match self.page {
                Page::Pairing => self.pairing_page(ui),
                Page::Menu => self.menu_page(ui),
                Page::Running => self.running_page(ui),
            });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.shutdown();
    }
}

// --------------------------------------------------------------- CJK font

/// egui's bundled fonts have no CJK glyphs, so load a system Chinese font.
fn install_cjk_font(ctx: &egui::Context) {
    const CANDIDATES: &[&str] = &[
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyh.ttf",
        r"C:\Windows\Fonts\msyhl.ttc",
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\simsun.ttc",
        r"C:\Windows\Fonts\Deng.ttf",
    ];

    for path in CANDIDATES {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        ctx.add_font(egui::FontInsert::new(
            "system-cjk",
            egui::FontData::from_owned(bytes),
            vec![
                egui::InsertFontFamily {
                    family: egui::FontFamily::Proportional,
                    priority: egui::FontPriority::Highest,
                },
                // keep Latin output monospaced, CJK falls back to this font
                egui::InsertFontFamily {
                    family: egui::FontFamily::Monospace,
                    priority: egui::FontPriority::Lowest,
                },
            ],
        ));
        return;
    }
}
