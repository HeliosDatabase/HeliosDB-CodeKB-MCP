//! Round-trip the user-level config TOML through write/read, and
//! exercise the longest-prefix lookup that hybrid mode relies on.
//!
//! Pure unit-style — does not open an `EmbeddedDatabase` or speak
//! MCP. The MCP-stdio integration test is a separate fixture (TODO,
//! pilot phase 2).

use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

// `#[path]` imports must live at the test crate's root so the
// `crate::kb` reference inside `config.rs` resolves — putting them
// inside a test fn makes them sibling modules of that fn instead of
// the crate root, breaking `crate::kb::*` lookups.
#[path = "../src/config.rs"]
mod config;
#[path = "../src/kb.rs"]
mod kb;

// Both tests below mutate process-global env vars (`XDG_CONFIG_HOME`
// / `XDG_DATA_HOME`).  Cargo runs tests in parallel by default, so
// without serialisation they race each other.  This Mutex is held
// for the lifetime of each test body.
static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn config_toml_round_trip_in_temp_xdg_home() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    // Force directories::ProjectDirs to land inside the tempdir.
    std::env::set_var("XDG_CONFIG_HOME", tmp.path().join("config"));
    std::env::set_var("XDG_DATA_HOME", tmp.path().join("data"));

    let mut cfg = config::Config::default();
    let source = tmp.path().join("repo");
    fs::create_dir_all(&source).unwrap();

    cfg.upsert_kb(
        &source,
        kb::KbSpec {
            mode: kb::KbMode::CoLocated,
            kb_dir: source.join(".helios-kb"),
        },
    );
    cfg.save().unwrap();

    let path = config::Config::path().unwrap();
    assert!(
        path.exists(),
        "expected config to land at {}",
        path.display()
    );

    let loaded = config::Config::load_or_default().unwrap();
    assert_eq!(loaded.kbs.len(), 1);
    let key = source
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(loaded.kbs.contains_key(&key));
    assert_eq!(loaded.kbs[&key].mode, kb::KbMode::CoLocated);
}

#[test]
fn longest_prefix_lookup_for_hybrid() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("XDG_CONFIG_HOME", tmp.path().join("config"));
    std::env::set_var("XDG_DATA_HOME", tmp.path().join("data"));

    let parent = tmp.path().join("Helios");
    let child = parent.join("Nano");
    fs::create_dir_all(&child).unwrap();

    let mut cfg = config::Config::default();
    // Hybrid KB at the parent, plus a more-specific co-located KB at
    // the child. Longest-prefix wins for queries inside `child`.
    cfg.upsert_kb(
        &parent,
        kb::KbSpec {
            mode: kb::KbMode::Hybrid,
            kb_dir: parent.join(".helios-kb"),
        },
    );
    cfg.upsert_kb(
        &child,
        kb::KbSpec {
            mode: kb::KbMode::CoLocated,
            kb_dir: child.join(".helios-kb"),
        },
    );

    let inside_child = child.join("src");
    fs::create_dir_all(&inside_child).unwrap();
    let resolved = cfg
        .lookup_for_source(&inside_child)
        .expect("expected child KB to win");
    let canonical_child = child.canonicalize().unwrap();
    assert_eq!(
        resolved.kb_dir,
        canonical_child.join(".helios-kb"),
        "longest-prefix match should pick the child KB"
    );

    // A path outside `child` but inside `parent` should fall back to
    // the parent (hybrid) KB.
    let other_child = parent.join("Lite");
    fs::create_dir_all(&other_child).unwrap();
    let resolved = cfg
        .lookup_for_source(&other_child)
        .expect("expected parent KB to handle non-child path");
    assert_eq!(resolved.mode, kb::KbMode::Hybrid);
}

// silence unused-PathBuf warning when the inner `mod` lookups
// aren't running (they always do, but clippy can't see through
// `#[path]`).
#[allow(dead_code)]
fn _force_use_pathbuf() -> PathBuf {
    PathBuf::new()
}
