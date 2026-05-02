//! Resume-on-interrupt checkpointing for cold ingest.
//!
//! Persists the ingest pipeline phase to `<kb_dir>/.ingest-state.json`
//! at each transition. If a process is killed mid-flight (Ctrl-C,
//! OOM, reboot) the next `ingest` run reads the file at startup and
//! skips already-completed phases. Cleared on successful completion.
//!
//! Per-file resume *within* the code-graph phase is already provided
//! by the engine's content-hash gate (`CodeIndexOptions::force_reparse
//! = false`): the indexer only re-processes files whose `_hdb_code_files`
//! row's `content_hash` differs from the source's current hash. That
//! works automatically since v3.21.0; the plugin's job is just to
//! avoid redoing the cheap walk + the doc projection when an
//! interrupted run had already cleared them.
//!
//! See ROADMAP Tier 1 #5.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const CHECKPOINT_FILE: &str = ".ingest-state.json";

/// Pipeline phases in execution order. Comparable so we can ask
/// "are we at-or-past phase X?".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// `walk_and_upsert` — building the `src` / `docs` tables.
    Walk,
    /// `code_index` — parsing + writing `_hdb_code_*`.
    CodeIndex,
    /// `graph_rag_ingest_docs` — projecting docs into the graph.
    GraphRag,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestCheckpoint {
    pub source_root: String,
    pub started_at_secs: u64,
    pub phase: Phase,
}

pub fn checkpoint_path(kb_dir: &Path) -> PathBuf {
    kb_dir.join(CHECKPOINT_FILE)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn read(kb_dir: &Path) -> Result<Option<IngestCheckpoint>> {
    let path = checkpoint_path(kb_dir);
    if !path.exists() {
        return Ok(None);
    }
    let body =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let cp: IngestCheckpoint =
        serde_json::from_str(&body).with_context(|| format!("parse {}", path.display()))?;
    Ok(Some(cp))
}

pub fn write(kb_dir: &Path, source_root: &str, phase: Phase, started_at_secs: u64) -> Result<()> {
    let cp = IngestCheckpoint {
        source_root: source_root.to_string(),
        started_at_secs,
        phase,
    };
    let path = checkpoint_path(kb_dir);
    let json = serde_json::to_string_pretty(&cp).context("serialise checkpoint")?;
    std::fs::write(&path, json).with_context(|| format!("write {}", path.display()))
}

/// Mark phase started fresh (no prior run) — sets `started_at_secs`
/// to now.  For phase advances within an existing run, callers should
/// preserve the original `started_at_secs` via `read()` then `write()`.
pub fn begin(kb_dir: &Path, source_root: &str, phase: Phase) -> Result<u64> {
    let now = now_secs();
    write(kb_dir, source_root, phase, now)?;
    Ok(now)
}

/// Move the checkpoint to the next phase, preserving `started_at_secs`.
pub fn advance(kb_dir: &Path, source_root: &str, phase: Phase) -> Result<()> {
    let started = read(kb_dir)?
        .map(|cp| cp.started_at_secs)
        .unwrap_or_else(now_secs);
    write(kb_dir, source_root, phase, started)
}

/// Clear the checkpoint on successful completion.
pub fn clear(kb_dir: &Path) -> Result<()> {
    let path = checkpoint_path(kb_dir);
    if path.exists() {
        std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
    }
    Ok(())
}
