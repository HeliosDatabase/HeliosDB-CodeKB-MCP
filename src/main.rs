//! `heliosdb-codekb-mcp` — MCP stdio server backed by an embedded
//! HeliosDB-Nano knowledge base.
//!
//! This binary is the consumer of the engine's `mcp-endpoint` /
//! `code-graph` / `graph-rag` / `code-embed` library features. It
//! does not modify the engine; it composes engine APIs with a
//! per-source KB-location config so a Claude Code (or other
//! MCP-aware) agent can query a single project, a global per-source
//! tree, or a hybrid multi-source aggregate.
//!
//! Subcommands:
//!   serve      run the stdio MCP loop bound to the source's KB
//!   init       create / configure a KB for a source path
//!   status     show config and per-KB stats
//!   config     get/set values in the user-level config TOML
//!
//! KB-location modes (decided at `init`):
//!   co-located    `<source>/.helios-kb/`
//!   global        `${XDG_DATA_HOME}/helios-kb/<slug>/`
//!   hybrid        explicit `--kb <PATH>` shared by multiple sources

#![allow(clippy::missing_errors_doc)]

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use heliosdb_nano::EmbeddedDatabase;

mod config;
mod ingest;
mod kb;
mod quality;

use config::Config;
use ingest::{ingest as run_ingest, open_kb_for_ingest, IngestOptions};
use kb::{KbMode, KbSpec};
use quality::{Phase, QualityProgress};

#[derive(Parser)]
#[command(
    name = "heliosdb-codekb-mcp",
    about = "MCP stdio server backed by an embedded HeliosDB-Nano KB."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the MCP server for the KB associated with `--source`.
    /// Default transport is stdio (the `.mcp.json` Claude Code path);
    /// pass `--http <addr>` to bind an HTTP/WebSocket/SSE endpoint
    /// instead — useful for Cursor and other clients that don't
    /// speak stdio.
    Serve {
        /// Absolute path of the source root the agent is working in.
        /// Used to look up the KB in the user-level config.
        #[arg(long)]
        source: PathBuf,

        /// Bind an HTTP MCP server on `<addr>` instead of running
        /// stdio. Routes mounted: POST `/` (JSON-RPC), GET `/ws`
        /// (WebSocket upgrade), GET `/sse` (server-sent events),
        /// GET `/info` (one-shot discovery + cache stats).
        /// Examples: `127.0.0.1:8765`, `0.0.0.0:8765`, `[::1]:8765`.
        #[arg(long, value_name = "ADDR")]
        http: Option<String>,
    },

    /// Create / configure a KB for a source path. Persists the choice
    /// in `~/.config/heliosdb-codekb-mcp/config.toml` and creates the
    /// KB data directory.
    Init {
        /// Source root (absolute path).
        #[arg(long)]
        source: PathBuf,

        /// KB-location mode: co-located | global | hybrid.
        #[arg(long, value_parser = ["co-located", "global", "hybrid"])]
        mode: String,

        /// Required for `--mode hybrid`: the explicit KB directory
        /// shared across sources. Optional override for `--mode global`.
        #[arg(long)]
        kb: Option<PathBuf>,

        /// Run a first ingest after the KB is created.
        #[arg(long)]
        ingest: bool,

        /// When ingesting, also extract text from PDFs / DOCX / XLSX
        /// (default tier — no Docling). Only used with `--ingest`.
        #[arg(long, action = clap::ArgAction::Set, default_value_t = true)]
        include_binary_docs: bool,

        /// When ingesting, force re-parse of every file (ignore the
        /// engine's content-hash gate).
        #[arg(long, default_value_t = false)]
        force: bool,

        /// Durable writes — fsync every write. Default off uses
        /// async WAL fsync (engine `WalSyncModeConfig::Async`,
        /// 10–100× throughput) since the index is regenerable from
        /// source.
        #[arg(long, default_value_t = false)]
        durable_writes: bool,

        /// Populate `body_vec` on `_hdb_code_symbols` using the
        /// in-process FastEmbedder (BGE-Small-EN-V1.5, 384-dim).
        /// First run downloads ~30 MB of model weights to
        /// `$XDG_CACHE_HOME/.fastembed_cache`. Lifts
        /// `helios_graphrag_search` quality for paraphrase-style
        /// queries. ROADMAP.md Tier 0.
        #[arg(long, default_value_t = false)]
        with_embeddings: bool,

        /// Fast pass first, then spawn a detached child for the
        /// embedding pass. User gets back control after the fast
        /// pass; queries already work (BM25 + hop-distance).
        /// Paraphrase quality comes online once the child finishes.
        /// Recommended for repos with >~1 k files. Implies
        /// `--with-embeddings` for the background phase. Track via
        /// `status --source <PWD>`.
        #[arg(long, default_value_t = false)]
        background_quality: bool,
    },

    /// Walk the source tree, classify and upsert files, run the
    /// code-graph indexer + graph-rag doc projection.
    Ingest {
        /// Source root (absolute path). Must already be registered
        /// via `init`.
        #[arg(long)]
        source: PathBuf,

        /// Also extract text from PDFs / DOCX / XLSX. Default on.
        #[arg(long, action = clap::ArgAction::Set, default_value_t = true)]
        include_binary_docs: bool,

        /// Force re-parse of every file (ignore the engine's
        /// content-hash gate).
        #[arg(long, default_value_t = false)]
        force: bool,

        /// Durable writes — fsync every write. Default off uses
        /// async WAL fsync (engine `WalSyncModeConfig::Async`,
        /// 10–100× throughput) since the index is regenerable from
        /// source. Set this if you want crash-safe durability and
        /// don't mind the slowdown.
        #[arg(long, default_value_t = false)]
        durable_writes: bool,

        /// Populate `body_vec` on `_hdb_code_symbols` using the
        /// in-process FastEmbedder (BGE-Small-EN-V1.5, 384-dim).
        /// First run downloads ~30 MB of model weights to
        /// `$XDG_CACHE_HOME/.fastembed_cache`. ROADMAP.md Tier 0.
        #[arg(long, default_value_t = false)]
        with_embeddings: bool,

        /// Fast pass first, then spawn a detached child for the
        /// embedding pass. User gets back control after the fast
        /// pass; queries already work (BM25 + hop-distance).
        /// Paraphrase quality comes online once the child finishes.
        /// Recommended for repos with >~1 k files. Implies
        /// `--with-embeddings` for the background phase. Track via
        /// `status --source <PWD>`.
        #[arg(long, default_value_t = false)]
        background_quality: bool,
    },

    /// Show config and per-KB stats. No `--source` ⇒ global summary.
    Status {
        #[arg(long)]
        source: Option<PathBuf>,
    },

    /// Read or update a value in the config TOML.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Print the resolved config TOML to stdout.
    Show,
    /// Set the default KB mode (used when `init` is not given a `--mode`).
    SetDefaultMode {
        #[arg(value_parser = ["co-located", "global", "hybrid"])]
        mode: String,
    },
    /// Print the path of the config file (creates it if missing).
    Path,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr) // never write tracing to stdout — MCP uses it
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Serve { source, http } => serve(&source, http.as_deref()).await,
        Commands::Init {
            source,
            mode,
            kb,
            ingest,
            include_binary_docs,
            force,
            durable_writes,
            with_embeddings,
            background_quality,
        } => {
            let mode = KbMode::parse(&mode)?;
            init(&source, mode, kb.as_deref())?;
            if ingest {
                let opts = IngestOptions {
                    source_root: source
                        .canonicalize()
                        .unwrap_or_else(|_| source.clone()),
                    include_binary_docs,
                    force_reparse: force,
                    durable_writes,
                    with_embeddings,
                    background_quality,
                };
                run_and_print_ingest(&opts)?;
            }
            Ok(())
        }
        Commands::Ingest {
            source,
            include_binary_docs,
            force,
            durable_writes,
            with_embeddings,
            background_quality,
        } => {
            let opts = IngestOptions {
                source_root: source.canonicalize()?,
                include_binary_docs,
                force_reparse: force,
                durable_writes,
                with_embeddings,
                background_quality,
            };
            run_and_print_ingest(&opts)
        }
        Commands::Status { source } => status(source.as_deref()),
        Commands::Config { action } => match action {
            ConfigAction::Show => {
                let cfg = Config::load_or_default()?;
                println!("{}", cfg.to_toml()?);
                Ok(())
            }
            ConfigAction::SetDefaultMode { mode } => {
                let mode = KbMode::parse(&mode)?;
                let mut cfg = Config::load_or_default()?;
                cfg.default_mode = mode;
                cfg.save()?;
                eprintln!("default-mode set to {}", mode.as_str());
                Ok(())
            }
            ConfigAction::Path => {
                println!("{}", Config::path()?.display());
                Ok(())
            }
        },
    }
}

async fn serve(source: &std::path::Path, http: Option<&str>) -> Result<()> {
    let cfg = Config::load_or_default()?;
    let spec = cfg
        .lookup_for_source(source)
        .with_context(|| format!(
            "no KB configured for source `{}`. Run `heliosdb-codekb-mcp init --source {} --mode <co-located|global|hybrid>` first.",
            source.display(),
            source.display(),
        ))?;

    tracing::info!(kb = %spec.kb_dir.display(), "opening KB");
    let db = Arc::new(EmbeddedDatabase::new(&spec.kb_dir).with_context(|| {
        format!("failed to open EmbeddedDatabase at {}", spec.kb_dir.display())
    })?);

    match http {
        None => {
            tracing::info!("starting MCP stdio server");
            let mut server = heliosdb_nano::mcp::McpServer::new(db);
            server
                .run()
                .await
                .map_err(|e| anyhow::anyhow!("MCP server failed: {e}"))
        }
        Some(addr) => {
            // Bind axum's MCP router on the requested address.  The
            // engine's `mcp_router` carries every transport variant
            // (POST `/`, GET `/ws`, GET `/sse`, GET `/info`) on a
            // single `Router<()>`, so we just hand it a fresh
            // `McpState` and call `axum::serve`.
            let state = heliosdb_nano::mcp::McpState::new(db);
            let app = heliosdb_nano::mcp::mcp_router(state);
            let listener = tokio::net::TcpListener::bind(addr)
                .await
                .with_context(|| format!("bind MCP HTTP listener on {addr}"))?;
            let bound = listener
                .local_addr()
                .map(|a| a.to_string())
                .unwrap_or_else(|_| addr.to_string());
            eprintln!("MCP HTTP server listening on http://{bound}");
            eprintln!("  POST /         JSON-RPC 2.0");
            eprintln!("  GET  /ws       WebSocket upgrade");
            eprintln!("  GET  /sse      server-sent events");
            eprintln!("  GET  /info     discovery + cache stats");
            tracing::info!(%bound, "starting MCP HTTP server");
            // Graceful shutdown on Ctrl-C / SIGTERM so a kill from
            // the agent harness doesn't strand RocksDB locks.
            let shutdown = async {
                let _ = tokio::signal::ctrl_c().await;
                tracing::info!("MCP HTTP server received Ctrl-C, shutting down");
            };
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown)
                .await
                .map_err(|e| anyhow::anyhow!("MCP HTTP server failed: {e}"))
        }
    }
}

fn init(source: &std::path::Path, mode: KbMode, kb_override: Option<&std::path::Path>) -> Result<()> {
    let source = source.canonicalize().with_context(|| {
        format!("source path `{}` must exist and be canonicalisable", source.display())
    })?;
    let spec = KbSpec::resolve(&source, mode, kb_override)?;

    std::fs::create_dir_all(&spec.kb_dir).with_context(|| {
        format!("failed to create KB directory {}", spec.kb_dir.display())
    })?;

    if mode == KbMode::CoLocated {
        ensure_gitignore_entry(&source, ".helios-kb/")?;
    }

    let mut cfg = Config::load_or_default()?;
    cfg.upsert_kb(&source, spec.clone());
    cfg.save()?;

    eprintln!("✓ KB created at {}", spec.kb_dir.display());
    eprintln!("✓ source `{}` → mode `{}`", source.display(), mode.as_str());
    if mode == KbMode::CoLocated {
        eprintln!("✓ `.helios-kb/` added to {}/.gitignore", source.display());
    }
    eprintln!("✓ config persisted at {}", Config::path()?.display());
    eprintln!();
    eprintln!("Next: register the MCP server with your agent and start a session.");
    Ok(())
}

fn status(source: Option<&std::path::Path>) -> Result<()> {
    let cfg = Config::load_or_default()?;
    if let Some(s) = source {
        let s = s.canonicalize()?;
        match cfg.lookup_for_source(&s) {
            Some(spec) => {
                println!("source : {}", s.display());
                println!("kb     : {}", spec.kb_dir.display());
                println!("mode   : {}", spec.mode.as_str());
                if let Ok(meta) = std::fs::metadata(&spec.kb_dir) {
                    println!("kb-on-disk : exists ({} bytes top-level)", meta.len());
                } else {
                    println!("kb-on-disk : missing — run `init` again");
                }
                print_quality_phase(&spec.kb_dir);
            }
            None => {
                println!("no KB configured for {}", s.display());
            }
        }
    } else {
        println!("config : {}", Config::path()?.display());
        println!("default-mode : {}", cfg.default_mode.as_str());
        println!("registered KBs ({}):", cfg.kbs.len());
        for (src, spec) in &cfg.kbs {
            println!("  {}  →  {}  ({})", src, spec.kb_dir.display(), spec.mode.as_str());
        }
    }
    Ok(())
}

/// Pretty-print the quality phase (background-embedding child) state.
/// No-op when no progress file exists.
fn print_quality_phase(kb_dir: &std::path::Path) {
    let path = quality::progress_path(kb_dir);
    let progress = match quality::read(&path) {
        Ok(p) => p,
        Err(e) => {
            println!("quality phase : (error reading {}: {e})", path.display());
            return;
        }
    };
    match quality::classify(progress) {
        Phase::NotStarted => {
            // Don't print anything — silence is the right default.
        }
        Phase::Running { p, alive } => {
            let now = quality::now_secs();
            let elapsed = now.saturating_sub(p.started_at_secs);
            if alive {
                println!(
                    "quality phase : running — pid {} ({} elapsed)",
                    p.pid,
                    quality::fmt_duration_secs(elapsed)
                );
            } else {
                println!(
                    "quality phase : stale — pid {} not running and no completion recorded",
                    p.pid
                );
                println!("              : tail {} or re-run `ingest --background-quality`", p.log_path);
            }
            println!("              : log → {}", p.log_path);
        }
        Phase::Complete { p } => {
            let took = p
                .completed_at_secs
                .unwrap_or(p.started_at_secs)
                .saturating_sub(p.started_at_secs);
            let now = quality::now_secs();
            let ago = now.saturating_sub(p.completed_at_secs.unwrap_or(now));
            println!(
                "quality phase : complete — took {}, finished {} ago",
                quality::fmt_duration_secs(took),
                quality::fmt_duration_secs(ago)
            );
        }
    }
}

fn run_and_print_ingest(opts: &IngestOptions) -> Result<()> {
    let cfg = Config::load_or_default()?;
    let spec = cfg.lookup_for_source(&opts.source_root).with_context(|| {
        format!(
            "no KB configured for source `{}`. Run `heliosdb-codekb-mcp init --source {} --mode <co-located|global|hybrid>` first.",
            opts.source_root.display(),
            opts.source_root.display(),
        )
    })?;

    // Detect "are we the detached background-quality child?" — the
    // parent sets `HELIOS_QUALITY_PROGRESS_FILE` on the child's env
    // when it spawns it.  The child runs the embedding pass inline
    // and finalises the progress file at the end.
    let is_quality_child = std::env::var(quality::PROGRESS_ENV).is_ok();

    // Choose the inline ingest options. Three cases:
    //   * Quality child  → with_embeddings=true, force_reparse=true.
    //   * Parent w/ bg-quality → with_embeddings=false (defer to
    //     child); user-specified force_reparse honoured.
    //   * Plain run → use the user's flags as-is.
    let inline_opts = if is_quality_child {
        IngestOptions {
            with_embeddings: true,
            force_reparse: true,
            background_quality: false,
            ..opts.clone()
        }
    } else if opts.background_quality {
        IngestOptions {
            with_embeddings: false,
            background_quality: false,
            ..opts.clone()
        }
    } else {
        IngestOptions {
            background_quality: false,
            ..opts.clone()
        }
    };

    let db = open_kb_for_ingest(&spec.kb_dir, opts.durable_writes)?;
    let summary = run_ingest(&db, inline_opts.clone())?;

    eprintln!("ingest summary");
    eprintln!("  source        : {}", opts.source_root.display());
    eprintln!("  kb            : {}", spec.kb_dir.display());
    eprintln!(
        "  wal sync      : {}",
        if opts.durable_writes { "Sync (durable)" } else { "Async (fast)" }
    );
    eprintln!("  files seen    : {}", summary.files_seen);
    eprintln!(
        "  upserted      : {} code, {} text, {} binary-doc",
        summary.code_upserts, summary.doc_upserts, summary.binary_upserts
    );
    eprintln!(
        "  skipped       : {}  read errors: {}",
        summary.skipped, summary.read_errors
    );
    eprintln!("  elapsed       : {} ms", summary.elapsed_ms);
    if !summary.read_error_samples.is_empty() {
        eprintln!(
            "  read error samples ({} of {}):",
            summary.read_error_samples.len(),
            summary.read_errors
        );
        for s in &summary.read_error_samples {
            eprintln!("    {s}");
        }
    }
    if let Some(c) = summary.code {
        eprintln!(
            "  code_index    : files_seen={} parsed={} unchanged={} skipped={} symbols={} refs={}",
            c.files_seen,
            c.files_parsed,
            c.files_unchanged,
            c.files_skipped,
            c.symbols_written,
            c.refs_written
        );
        // Engine v3.21.0+ per-phase timings + parallelism telemetry.
        // Lets operators see speedup directly instead of stopwatching.
        eprintln!(
            "  code_index ms : parse={} write={} workers={} chunks={}",
            c.parse_elapsed_ms,
            c.write_elapsed_ms,
            c.parse_workers,
            c.chunks_processed
        );
    }
    if let Some(d) = summary.docs {
        eprintln!(
            "  graph_rag     : nodes={} edges={} rows_seen={} rows_skipped={}",
            d.nodes_added, d.edges_added, d.rows_seen, d.rows_skipped
        );
    }

    // Release the embedded DB before either finalising progress or
    // forking a child — the engine doesn't yet support concurrent
    // multi-process writes (FR #9 in ROADMAP, deferred).
    drop(db);

    if is_quality_child {
        // We are the background child; mark the progress file complete.
        let progress_path = quality::progress_path(&spec.kb_dir);
        quality::finalize(&progress_path)
            .with_context(|| format!("finalise {}", progress_path.display()))?;
    } else if opts.background_quality {
        spawn_quality_child(&opts.source_root, &spec.kb_dir, opts.durable_writes)?;
    }

    Ok(())
}

/// Fork a detached `heliosdb-codekb-mcp ingest --with-embeddings
/// --force` child. Parent returns immediately. Child writes progress
/// to `<kb_dir>/quality-progress.json` and stderr to
/// `<kb_dir>/quality.log`; `setsid(2)` puts it in its own session
/// so the user closing the launching TTY doesn't SIGHUP it.
fn spawn_quality_child(
    source_root: &std::path::Path,
    kb_dir: &std::path::Path,
    durable_writes: bool,
) -> Result<()> {
    let progress_path = quality::progress_path(kb_dir);
    let log_path = quality::log_path(kb_dir);

    // Truncate previous log (one fresh log per quality run).
    let log_file = std::fs::File::create(&log_path)
        .with_context(|| format!("create {}", log_path.display()))?;
    let stderr_file = log_file.try_clone()?;

    let exe = std::env::current_exe().context("locate current_exe")?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("ingest")
        .arg("--source").arg(source_root)
        .arg("--with-embeddings")
        .arg("--force");
    if durable_writes {
        cmd.arg("--durable-writes");
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(log_file)
        .stderr(stderr_file)
        .env(quality::PROGRESS_ENV, &progress_path);

    // setsid(2) — child becomes its own session leader, detached
    // from the parent's controlling TTY. Without this the child
    // dies on SIGHUP when the user closes the launching shell.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    let child = cmd.spawn().context("spawn quality child")?;
    let pid = child.id();

    // Initial progress JSON — parent owns it; child finalises.
    let progress = QualityProgress {
        pid,
        started_at_secs: quality::now_secs(),
        completed_at_secs: None,
        log_path: log_path.to_string_lossy().into_owned(),
        source_root: source_root.to_string_lossy().into_owned(),
    };
    quality::write(&progress_path, &progress)
        .with_context(|| format!("write {}", progress_path.display()))?;

    eprintln!();
    eprintln!("background quality phase started:");
    eprintln!("  pid       : {pid}");
    eprintln!("  log       : {}", log_path.display());
    eprintln!("  progress  : {}", progress_path.display());
    eprintln!();
    eprintln!("Track via:");
    eprintln!("  heliosdb-codekb-mcp status --source {}", source_root.display());
    eprintln!();
    eprintln!("MCP queries can already use the index (BM25 + hop-distance);");
    eprintln!("paraphrase quality lifts once the embedding pass finishes.");

    // Don't wait. Detach by dropping the Child handle.
    drop(child);
    Ok(())
}

fn ensure_gitignore_entry(repo_root: &std::path::Path, entry: &str) -> Result<()> {
    let path = repo_root.join(".gitignore");
    let body = std::fs::read_to_string(&path).unwrap_or_default();
    if body.lines().any(|l| l.trim() == entry.trim_end_matches('/') || l.trim() == entry) {
        return Ok(());
    }
    let mut new = body;
    if !new.is_empty() && !new.ends_with('\n') {
        new.push('\n');
    }
    new.push_str(entry);
    new.push('\n');
    std::fs::write(&path, new)
        .with_context(|| format!("failed to update {}", path.display()))?;
    Ok(())
}
