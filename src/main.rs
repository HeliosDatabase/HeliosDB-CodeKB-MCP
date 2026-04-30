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

use config::Config;
use ingest::{ingest as run_ingest, open_kb_for_ingest, IngestOptions};
use kb::{KbMode, KbSpec};

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
    /// Run the stdio MCP loop for the KB associated with `--source`.
    Serve {
        /// Absolute path of the source root the agent is working in.
        /// Used to look up the KB in the user-level config.
        #[arg(long)]
        source: PathBuf,
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
        Commands::Serve { source } => serve(&source).await,
        Commands::Init {
            source,
            mode,
            kb,
            ingest,
            include_binary_docs,
            force,
            durable_writes,
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
        } => {
            let opts = IngestOptions {
                source_root: source.canonicalize()?,
                include_binary_docs,
                force_reparse: force,
                durable_writes,
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

async fn serve(source: &std::path::Path) -> Result<()> {
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

    tracing::info!("starting MCP stdio server");
    let mut server = heliosdb_nano::mcp::McpServer::new(db);
    server
        .run()
        .await
        .map_err(|e| anyhow::anyhow!("MCP server failed: {e}"))
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

fn run_and_print_ingest(opts: &IngestOptions) -> Result<()> {
    let cfg = Config::load_or_default()?;
    let spec = cfg.lookup_for_source(&opts.source_root).with_context(|| {
        format!(
            "no KB configured for source `{}`. Run `heliosdb-codekb-mcp init --source {} --mode <co-located|global|hybrid>` first.",
            opts.source_root.display(),
            opts.source_root.display(),
        )
    })?;
    let db = open_kb_for_ingest(&spec.kb_dir, opts.durable_writes)?;
    let summary = run_ingest(&db, opts.clone())?;

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
