//! Background-quality phase progress file.
//!
//! When a user runs `ingest --background-quality`, the parent does
//! the fast pass synchronously, then spawns a detached child to do
//! the embedding pass (`code_index` with `embed_bodies = true,
//! force_reparse = true`). The parent writes this struct to
//! `<kb_dir>/quality-progress.json`; the child finalises it with
//! `completed_at_secs` on success. The `status` command reads it.
//!
//! There is no real-time per-row progress (the engine runs the
//! embedding pass as one long call); the file's job is "is the
//! background phase running, complete, or dead?". The child's
//! stderr lands in `<kb_dir>/quality.log` for the curious.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const PROGRESS_FILE: &str = "quality-progress.json";
pub const LOG_FILE: &str = "quality.log";
/// Env var the parent sets on the child so the child knows it should
/// finalise the progress file on success (instead of recursing into
/// another `--background-quality` spawn).
pub const PROGRESS_ENV: &str = "HELIOS_QUALITY_PROGRESS_FILE";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityProgress {
    pub pid: u32,
    pub started_at_secs: u64,
    pub completed_at_secs: Option<u64>,
    pub log_path: String,
    pub source_root: String,
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn progress_path(kb_dir: &Path) -> PathBuf {
    kb_dir.join(PROGRESS_FILE)
}

pub fn log_path(kb_dir: &Path) -> PathBuf {
    kb_dir.join(LOG_FILE)
}

pub fn write(path: &Path, p: &QualityProgress) -> Result<()> {
    let json = serde_json::to_string_pretty(p)
        .context("serialise QualityProgress")?;
    std::fs::write(path, json)
        .with_context(|| format!("write {}", path.display()))
}

pub fn read(path: &Path) -> Result<Option<QualityProgress>> {
    if !path.exists() {
        return Ok(None);
    }
    let body = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    let p: QualityProgress = serde_json::from_str(&body)
        .with_context(|| format!("parse {}", path.display()))?;
    Ok(Some(p))
}

/// Mark the progress file complete. Called by the child at the end
/// of a successful ingest run when `PROGRESS_ENV` is set.
pub fn finalize(path: &Path) -> Result<()> {
    if let Some(mut p) = read(path)? {
        p.completed_at_secs = Some(now_secs());
        write(path, &p)?;
    }
    Ok(())
}

/// Linux-only: is `pid` still a running process? Falls back to
/// `kill(pid, 0)` on other Unix; non-Unix returns false.
pub fn pid_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// Phase summary for the status command.
pub enum Phase {
    NotStarted,
    Running { p: QualityProgress, alive: bool },
    Complete { p: QualityProgress },
}

pub fn classify(p: Option<QualityProgress>) -> Phase {
    match p {
        None => Phase::NotStarted,
        Some(p) if p.completed_at_secs.is_some() => Phase::Complete { p },
        Some(p) => {
            let alive = pid_alive(p.pid);
            Phase::Running { p, alive }
        }
    }
}

/// Human-friendly "Xm Ys ago" / "Xm Ys" duration formatter.
pub fn fmt_duration_secs(s: u64) -> String {
    let m = s / 60;
    let r = s % 60;
    if m == 0 {
        format!("{r} s")
    } else if m < 60 {
        format!("{m} m {r} s")
    } else {
        let h = m / 60;
        let m = m % 60;
        format!("{h} h {m} m")
    }
}
