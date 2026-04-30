//! KB-location resolver.
//!
//! Three modes:
//!
//! * `co-located`  — KB at `<source>/.helios-kb`. The init flow adds
//!                   `.helios-kb/` to `<source>/.gitignore`.
//! * `global`      — KB at `${XDG_DATA_HOME:-~/.local/share}/helios-kb/<slug>`
//!                   where `<slug>` is a path-encoded form of the
//!                   absolute source path.
//! * `hybrid`      — KB at an explicit path the user provides via
//!                   `--kb`; multiple sources can share the same
//!                   directory by registering each via `init`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KbMode {
    CoLocated,
    Global,
    Hybrid,
}

impl KbMode {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "co-located" | "colocated" => Ok(Self::CoLocated),
            "global" => Ok(Self::Global),
            "hybrid" => Ok(Self::Hybrid),
            other => bail!("unknown KB mode `{other}`. Expected co-located, global, or hybrid."),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CoLocated => "co-located",
            Self::Global => "global",
            Self::Hybrid => "hybrid",
        }
    }
}

#[derive(Debug, Clone)]
pub struct KbSpec {
    pub mode: KbMode,
    pub kb_dir: PathBuf,
}

impl KbSpec {
    pub fn resolve(source: &Path, mode: KbMode, kb_override: Option<&Path>) -> Result<Self> {
        let kb_dir = match (mode, kb_override) {
            (KbMode::CoLocated, None) => source.join(".helios-kb"),
            (KbMode::CoLocated, Some(_)) => {
                bail!("--kb is not used with --mode co-located; the KB lives at <source>/.helios-kb")
            }
            (KbMode::Global, None) => global_default_kb_dir(source)?,
            (KbMode::Global, Some(p)) => p.to_path_buf(),
            (KbMode::Hybrid, Some(p)) => p.to_path_buf(),
            (KbMode::Hybrid, None) => {
                bail!("--mode hybrid requires --kb <PATH> (the shared KB directory)")
            }
        };
        Ok(Self { mode, kb_dir })
    }
}

fn global_default_kb_dir(source: &Path) -> Result<PathBuf> {
    let pd = ProjectDirs::from("", "", "helios-kb")
        .ok_or_else(|| anyhow!("could not resolve XDG data dir"))?;
    let root = pd.data_dir();
    std::fs::create_dir_all(root)
        .with_context(|| format!("failed to create {}", root.display()))?;
    Ok(root.join(slugify(source)))
}

fn slugify(p: &Path) -> String {
    let s = p.to_string_lossy();
    s.trim_start_matches('/')
        .replace(['/', '\\'], "-")
        .replace(' ', "_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parse_round_trip() {
        for v in ["co-located", "global", "hybrid"] {
            let m = KbMode::parse(v).unwrap();
            assert_eq!(m.as_str(), v);
        }
    }

    #[test]
    fn parse_unknown() {
        assert!(KbMode::parse("nope").is_err());
    }

    #[test]
    fn co_located_path() {
        let s = PathBuf::from("/work/repo");
        let spec = KbSpec::resolve(&s, KbMode::CoLocated, None).unwrap();
        assert_eq!(spec.kb_dir, PathBuf::from("/work/repo/.helios-kb"));
    }

    #[test]
    fn co_located_rejects_kb_override() {
        let s = PathBuf::from("/work/repo");
        assert!(KbSpec::resolve(&s, KbMode::CoLocated, Some(Path::new("/x"))).is_err());
    }

    #[test]
    fn hybrid_requires_kb() {
        let s = PathBuf::from("/work/repo");
        assert!(KbSpec::resolve(&s, KbMode::Hybrid, None).is_err());
        let spec = KbSpec::resolve(&s, KbMode::Hybrid, Some(Path::new("/shared"))).unwrap();
        assert_eq!(spec.kb_dir, PathBuf::from("/shared"));
    }

    #[test]
    fn slugify_strips_leading_slash() {
        assert_eq!(slugify(Path::new("/home/me/proj")), "home-me-proj");
    }
}
