//! User-level config at `${XDG_CONFIG_HOME:-~/.config}/heliosdb-codekb-mcp/config.toml`.
//!
//! Schema:
//!
//! ```toml
//! default_mode = "global"            # used when `init` is invoked without --mode
//!
//! [serve]
//! profile = "standard"               # minimal | standard | full (default standard)
//! strip_tool_descriptions = "200"    # int | "none" | "all"
//!
//! [kbs."/abs/path/to/source"]
//! mode     = "co-located"
//! kb_dir   = "/abs/path/to/source/.helios-kb"
//! created  = "2026-04-27T14:32:18Z"
//! ```
//!
//! Keys in `[kbs.…]` are absolute, canonicalised source paths. The
//! same `kb_dir` may appear under multiple keys when the user picked
//! `mode = "hybrid"` to share one KB across sources.
//!
//! The `[serve]` section is consumed by `Commands::Serve` as a default
//! for the `--profile` / `--strip-tool-descriptions` flags. CLI args
//! always win; config TOML is the second-priority source; the
//! built-in defaults (`standard` / `200`) are the last fallback.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::kb::{KbMode, KbSpec};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_mode_default")]
    pub default_mode: KbMode,
    #[serde(default)]
    pub serve: ServeConfig,
    #[serde(default)]
    pub kbs: BTreeMap<String, StoredKb>,
}

fn default_mode_default() -> KbMode {
    KbMode::Global
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredKb {
    pub mode: KbMode,
    pub kb_dir: PathBuf,
    #[serde(default)]
    pub created: String,
}

/// User-level defaults for `Commands::Serve`. CLI flags override these.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServeConfig {
    /// MCP tool-surface profile: `minimal` | `standard` | `full`.
    /// `None` ⇒ binary falls back to `standard`.
    pub profile: Option<String>,
    /// How much of each tool's `description` to keep in `tools/list`.
    /// Accepts an integer (cap at N bytes), `"none"`, or `"all"`.
    /// `None` ⇒ binary falls back to `"200"`.
    pub strip_tool_descriptions: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_mode: KbMode::Global,
            serve: ServeConfig::default(),
            kbs: BTreeMap::new(),
        }
    }
}

impl Config {
    pub fn path() -> Result<PathBuf> {
        let pd = ProjectDirs::from("", "", "heliosdb-codekb-mcp")
            .context("could not resolve config directory")?;
        let dir = pd.config_dir();
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
        Ok(dir.join("config.toml"))
    }

    pub fn load_or_default() -> Result<Self> {
        let path = Self::path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        let text = self.to_toml()?;
        std::fs::write(&path, text)
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }

    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).context("failed to serialise config")
    }

    pub fn upsert_kb(&mut self, source: &Path, spec: KbSpec) {
        let key = source.to_string_lossy().into_owned();
        self.kbs.insert(
            key,
            StoredKb {
                mode: spec.mode,
                kb_dir: spec.kb_dir,
                created: now_iso(),
            },
        );
    }

    /// Return the KB spec for the most-specific source ancestor of
    /// `query`. Allows a user to register a hybrid KB at
    /// `/home/me/work` and have any sub-tree query resolve through it.
    pub fn lookup_for_source(&self, query: &Path) -> Option<KbSpec> {
        let q = query.canonicalize().ok()?;
        let q_str = q.to_string_lossy();
        let mut best: Option<(&String, &StoredKb)> = None;
        for (k, v) in &self.kbs {
            if q_str == k.as_str() || q_str.starts_with(&format!("{k}/")) {
                let take = match best {
                    Some((bk, _)) => k.len() > bk.len(),
                    None => true,
                };
                if take {
                    best = Some((k, v));
                }
            }
        }
        best.map(|(_, v)| KbSpec {
            mode: v.mode,
            kb_dir: v.kb_dir.clone(),
        })
    }
}

fn now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Lightweight ISO-8601 from epoch seconds (UTC), no chrono dep.
    let (y, mo, d, h, mi, s) = epoch_to_ymdhms(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

fn epoch_to_ymdhms(mut s: u64) -> (i32, u32, u32, u32, u32, u32) {
    let sec = (s % 60) as u32;
    s /= 60;
    let mi = (s % 60) as u32;
    s /= 60;
    let h = (s % 24) as u32;
    s /= 24;
    // days since 1970-01-01
    let mut days = s as i64;
    let mut year = 1970i32;
    loop {
        let leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
        let dy = if leap { 366 } else { 365 };
        if days < dy {
            break;
        }
        days -= dy;
        year += 1;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
    let months = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut mo = 1u32;
    for &dim in &months {
        if days < dim {
            break;
        }
        days -= dim;
        mo += 1;
    }
    (year, mo, (days + 1) as u32, h, mi, sec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn default_round_trip() {
        let cfg = Config::default();
        let s = cfg.to_toml().unwrap();
        let parsed: Config = toml::from_str(&s).unwrap();
        assert_eq!(parsed.default_mode, KbMode::Global);
        assert!(parsed.kbs.is_empty());
    }

    #[test]
    fn serve_section_round_trips() {
        let mut cfg = Config::default();
        cfg.serve.profile = Some("minimal".to_string());
        cfg.serve.strip_tool_descriptions = Some("all".to_string());
        let s = cfg.to_toml().unwrap();
        let parsed: Config = toml::from_str(&s).unwrap();
        assert_eq!(parsed.serve.profile.as_deref(), Some("minimal"));
        assert_eq!(parsed.serve.strip_tool_descriptions.as_deref(), Some("all"));
    }

    #[test]
    fn missing_serve_section_is_default() {
        // Pre-Layer-1 configs have no [serve] section. Parse must
        // succeed and leave the fields as None so the binary uses
        // built-in defaults.
        let legacy = r#"default_mode = "global""#;
        let parsed: Config = toml::from_str(legacy).unwrap();
        assert!(parsed.serve.profile.is_none());
        assert!(parsed.serve.strip_tool_descriptions.is_none());
    }

    #[test]
    fn upsert_and_lookup() {
        let mut cfg = Config::default();
        cfg.upsert_kb(
            &PathBuf::from("/tmp/example-repo"),
            KbSpec {
                mode: KbMode::CoLocated,
                kb_dir: PathBuf::from("/tmp/example-repo/.helios-kb"),
            },
        );
        // direct hit; canonicalisation in lookup_for_source means we
        // need the path to exist on disk for that branch — testing
        // the storage shape only here.
        assert_eq!(cfg.kbs.len(), 1);
        assert!(cfg.kbs.contains_key("/tmp/example-repo"));
    }
}
