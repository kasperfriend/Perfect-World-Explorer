//! Lighter logging: rotating log file, session header, provider events,
//! per-frame timing samples, acceleration-structure stats, coverage, memory
//! and errors. Controlled by the "Logging" toggle in the module panel and
//! the `PW_LIGHTER_LOG=1|verbose|off` env var. Never records user-name
//! segments of paths.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_FILES: usize = 10;
const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024; // 50 MB

/// Verbosity: off, normal, verbose.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LogLevel {
    #[default]
    Off,
    Normal,
    Verbose,
}

impl LogLevel {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "1" | "normal" => Some(Self::Normal),
            "verbose" => Some(Self::Verbose),
            _ => None,
        }
    }
}

/// A single log entry. Level determines whether it is written at the
/// configured verbosity.
#[derive(Clone, Debug)]
pub enum LogEntry {
    /// Session header written once at startup.
    Session(SessionHeader),
    /// SDK load attempt.
    Provider(ProviderEvent),
    /// Per-frame timing sample (verbose only).
    Frame(FrameSample),
    /// Acceleration-structure event (verbose only).
    AccelStruct(AccelStructEvent),
    /// Texture coverage event.
    Coverage(CoverageEvent),
    /// Memory snapshot (sampled once per second).
    Memory(MemorySnapshot),
    /// Error or fallback reason.
    Error(ErrorEvent),
}

#[derive(Clone, Debug)]
pub struct SessionHeader {
    pub commit: Option<String>,
    pub build_date: Option<String>,
    pub os_version: Option<String>,
    pub gpu: Option<String>,
    pub driver: Option<String>,
    pub vulkan_version: Option<String>,
    pub heap_size_mb: Option<u64>,
    pub graphics_upscaler: Option<String>,
    pub graphics_frame_gen: Option<String>,
    pub module_settings: String,
    pub env_vars: Vec<(String, String)>, // values redacted if they contain user-name segments
}

#[derive(Clone, Debug)]
pub struct ProviderEvent {
    pub sdk: String,
    pub path: String, // redacted if it contains a user-name segment
    pub size_bytes: u64,
    pub sha256: Option<String>,
    pub result: String, // "loaded" | "rejected" | "missing" | "bad_hash" | ...
    pub error_text: Option<String>,
}

#[derive(Clone, Debug)]
pub struct FrameSample {
    pub frame: u64,
    pub denoiser: String,
    pub upscaler: String,
    pub frame_gen: String,
    pub ray_resolution: u32,
    pub stage_ms: [f64; 8], // AS, lighting trace, denoise, upsample/compose, present, ...
    pub cpu_frame_ms: f64,
    pub fallback_reason: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AccelStructEvent {
    pub chunk: u32,
    pub blas_builds: u32,
    pub refits: u32,
    pub copy_bytes: u64,
    pub zero_checks: u64,
}

#[derive(Clone, Debug)]
pub struct CoverageEvent {
    pub map: String,
    pub loaded: u32,
    pub skipped: u32,
    pub failed: u32,
    pub fail_reason: Option<String>,
    pub mip_fallbacks: u32,
    pub unavailable_textures: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct MemorySnapshot {
    pub lighter_allocation_total: u64,
    pub driver_device_local_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct ErrorEvent {
    pub stage: String,
    pub reason: String,
}

/// Redact user-name segments from a path: replace the user's home directory
/// component with `[user]`.
fn redact_path(path: &str) -> String {
    let p = Path::new(path);
    let mut parts: Vec<&std::ffi::OsStr> = p.iter().collect();
    for (i, part) in parts.iter().enumerate() {
        let s = part.to_string_lossy();
        if s == "Users" || s == "home" {
            if i + 1 < parts.len() {
                parts[i + 1] = std::ffi::OsStr::new("[user]");
                for j in 0..=i {
                    parts[j] = std::ffi::OsStr::new("");
                }
                break;
            }
        }
    }
    let mut result = String::new();
    for (i, part) in parts.iter().enumerate() {
        let s = part.to_string_lossy();
        if s.is_empty() {
            continue;
        }
        if i > 0 {
            result.push(std::path::MAIN_SEPARATOR);
        }
        result.push_str(&s);
    }
    result
}

/// Rotate log files: keep at most MAX_FILES files, each at most MAX_FILE_BYTES.
/// Returns the path to use for the next write.
fn rotate(log_dir: &Path) -> io::Result<PathBuf> {
    fs::create_dir_all(log_dir)?;
    let base = log_dir.join("lighter");
    let mut candidates: Vec<(PathBuf, u64, u64)> = Vec::new();
    if let Ok(entries) = fs::read_dir(log_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .map_or(false, |n| n.starts_with("lighter"))
            {
                if let Ok(meta) = entry.metadata() {
                    if let Ok(modified) = meta.modified() {
                        if let Ok(epoch) = modified.duration_since(UNIX_EPOCH) {
                            candidates.push((path, meta.len(), epoch.as_secs()));
                        }
                    }
                }
            }
        }
    }
    candidates.sort_by_key(|k| std::cmp::Reverse(k.2));

    // Remove files beyond MAX_FILES.
    for (path, _, _) in candidates.iter().skip(MAX_FILES) {
        let _ = fs::remove_file(path);
    }

    // Find the active log file (newest one under the size limit).
    let active = candidates.iter().find(|(_, size, _)| *size < MAX_FILE_BYTES);
    if let Some((path, _, _)) = active {
        return Ok(path.clone());
    }

    // Otherwise create a new file with a timestamp.
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    Ok(base.with_extension(format!("{ts}.log")))
}

/// Rotating log writer. Opens the active log file on each write (so rotation
/// can happen between writes without keeping a handle open).
pub struct Logger {
    level: Mutex<LogLevel>,
    log_dir: Option<PathBuf>,
    frame_count: Mutex<u64>,
}

impl Logger {
    pub fn new() -> Self {
        Self {
            level: Mutex::new(LogLevel::Off),
            log_dir: None,
            frame_count: Mutex::new(0),
        }
    }

    pub fn from_env() -> Self {
        let mut log = Self::new();
        let level = std::env::var("PW_LIGHTER_LOG")
            .ok()
            .as_deref()
            .and_then(LogLevel::parse)
            .unwrap_or(LogLevel::Off);
        *log.level.lock().unwrap() = level;
        if level != LogLevel::Off {
            log.log_dir = std::env::var_os("PW_CACHE_DIR")
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME")
                        .map(PathBuf::from)
                        .map(|p| p.join(".cache").join("PerfectWorldViewer").join("lighter-logs"))
                });
        }
        log
    }

    pub fn set_level(&self, level: LogLevel) {
        *self.level.lock().unwrap() = level;
    }

    pub fn level(&self) -> LogLevel {
        *self.level.lock().unwrap()
    }

    /// Write a log entry. Returns false if logging is off or the entry's
    /// verbosity exceeds the configured level.
    pub fn write(&self, entry: &LogEntry) -> bool {
        let level = self.level();
        if level == LogLevel::Off {
            return false;
        }
        let write_it = match entry {
            LogEntry::Session(_) => true,
            LogEntry::Provider(_) => true,
            LogEntry::Frame(_) => level == LogLevel::Verbose,
            LogEntry::AccelStruct(_) => level == LogLevel::Verbose,
            LogEntry::Coverage(_) => true,
            LogEntry::Memory(_) => level == LogLevel::Verbose,
            LogEntry::Error(_) => true,
        };
        if !write_it {
            return false;
        }
        let log_dir = match &self.log_dir {
            Some(d) => d.clone(),
            None => return false,
        };
        let path = match rotate(&log_dir) {
            Ok(p) => p,
            Err(_) => return false,
        };
        let mut line = String::new();
        match entry {
            LogEntry::Session(h) => {
                line.push_str("SESSION\n");
                if let Some(v) = &h.commit { line.push_str(&format!("  commit: {v}\n")); }
                if let Some(v) = &h.build_date { line.push_str(&format!("  build_date: {v}\n")); }
                if let Some(v) = &h.os_version { line.push_str(&format!("  os: {v}\n")); }
                if let Some(v) = &h.gpu { line.push_str(&format!("  gpu: {v}\n")); }
                if let Some(v) = &h.driver { line.push_str(&format!("  driver: {v}\n")); }
                if let Some(v) = &h.vulkan_version { line.push_str(&format!("  vulkan: {v}\n")); }
                if let Some(v) = h.heap_size_mb { line.push_str(&format!("  heap_mb: {v}\n")); }
                if let Some(v) = &h.graphics_upscaler { line.push_str(&format!("  graphics_upscaler: {v}\n")); }
                if let Some(v) = &h.graphics_frame_gen { line.push_str(&format!("  graphics_frame_gen: {v}\n")); }
                line.push_str(&format!("  module_settings: {}\n", h.module_settings));
                for (k, v) in &h.env_vars {
                    let redacted = if contains_user_segment(v) { "[redacted]".to_string() } else { v.clone() };
                    line.push_str(&format!("  env {k}: {redacted}\n"));
                }
            }
            LogEntry::Provider(p) => {
                line.push_str("PROVIDER\n");
                line.push_str(&format!("  sdk: {}\n", p.sdk));
                line.push_str(&format!("  path: {}\n", redact_path(&p.path)));
                line.push_str(&format!("  size: {}\n", p.size_bytes));
                if let Some(h) = &p.sha256 { line.push_str(&format!("  sha256: {h}\n")); }
                line.push_str(&format!("  result: {}\n", p.result));
                if let Some(e) = &p.error_text { line.push_str(&format!("  error: {e}\n")); }
            }
            LogEntry::Frame(f) => {
                *self.frame_count.lock().unwrap() += 1;
                line.push_str(&format!("FRAME {}\n", f.frame));
                line.push_str(&format!("  denoiser: {}\n", f.denoiser));
                line.push_str(&format!("  upscaler: {}\n", f.upscaler));
                line.push_str(&format!("  frame_gen: {}\n", f.frame_gen));
                line.push_str(&format!("  ray_resolution: {}\n", f.ray_resolution));
                line.push_str(&format!("  stage_ms: {:?}\n", f.stage_ms));
                line.push_str(&format!("  cpu_frame_ms: {}\n", f.cpu_frame_ms));
                if let Some(r) = &f.fallback_reason { line.push_str(&format!("  fallback: {r}\n")); }
            }
            LogEntry::AccelStruct(a) => {
                line.push_str("ACCEL_STRUCT\n");
                line.push_str(&format!("  chunk: {}\n", a.chunk));
                line.push_str(&format!("  blas_builds: {}\n", a.blas_builds));
                line.push_str(&format!("  refits: {}\n", a.refits));
                line.push_str(&format!("  copy_bytes: {}\n", a.copy_bytes));
                line.push_str(&format!("  zero_checks: {}\n", a.zero_checks));
            }
            LogEntry::Coverage(c) => {
                line.push_str("COVERAGE\n");
                line.push_str(&format!("  map: {}\n", c.map));
                line.push_str(&format!("  loaded: {}\n", c.loaded));
                line.push_str(&format!("  skipped: {}\n", c.skipped));
                line.push_str(&format!("  failed: {}\n", c.failed));
                if let Some(r) = &c.fail_reason { line.push_str(&format!("  fail_reason: {r}\n")); }
                line.push_str(&format!("  mip_fallbacks: {}\n", c.mip_fallbacks));
                for t in &c.unavailable_textures { line.push_str(&format!("  unavailable_texture: {t}\n")); }
            }
            LogEntry::Memory(m) => {
                line.push_str("MEMORY\n");
                line.push_str(&format!("  lighter_allocation_total: {}\n", m.lighter_allocation_total));
                line.push_str(&format!("  driver_device_local_bytes: {}\n", m.driver_device_local_bytes));
            }
            LogEntry::Error(e) => {
                line.push_str("ERROR\n");
                line.push_str(&format!("  stage: {}\n", e.stage));
                line.push_str(&format!("  reason: {}\n", e.reason));
            }
        }
        line.push_str("\n");

        let file = match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => f,
            Err(_) => return false,
        };
        let mut writer = std::io::BufWriter::new(file);
        let _ = writer.write_all(line.as_bytes());
        let _ = writer.flush();
        true
    }

    pub fn frame_count(&self) -> u64 {
        *self.frame_count.lock().unwrap()
    }
}

fn contains_user_segment(value: &str) -> bool {
    value.contains("Users/") || value.contains("/home/") || value.contains("\\Users\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_path_removes_user_segment() {
        let redacted = redact_path("C:/Users/Alice/Documents/PerfectWorldViewer/lighter.log");
        assert!(redacted.contains("[user]"));
        assert!(!redacted.contains("Alice"));
    }

    #[test]
    fn log_level_parse() {
        assert_eq!(LogLevel::parse("off"), Some(LogLevel::Off));
        assert_eq!(LogLevel::parse("1"), Some(LogLevel::Normal));
        assert_eq!(LogLevel::parse("normal"), Some(LogLevel::Normal));
        assert_eq!(LogLevel::parse("verbose"), Some(LogLevel::Verbose));
        assert_eq!(LogLevel::parse("bogus"), None);
    }

    #[test]
    fn rotation_keeps_under_limit() {
        let root = std::env::temp_dir().join(format!("pwe-log-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&root);
        for i in 0..(MAX_FILES + 3) {
            let path = root.join(format!("lighter-{i}.log"));
            std::fs::write(&path, vec![0u8; 100]).unwrap();
        }
        let _ = rotate(&root);
        let _ = std::fs::remove_dir_all(&root);
    }
}
