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

mod checkpoint;
mod config;
mod distill;
mod ingest;
mod kb;
mod linker;
mod mcp_trim;
mod quality;
mod wrappers;

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

        /// Cap each string field in a `tools/call` response at N bytes
        /// (UTF-8 char-boundary safe). Larger strings get an
        /// `…[+N bytes truncated]` marker so the agent knows what was
        /// dropped. `0` disables trimming (the engine's full response
        /// passes through unchanged). Honoured on both stdio and HTTP
        /// transports.
        ///
        /// Why this exists: `helios_lsp_*` and `helios_graphrag_search`
        /// return neighbouring-symbol bodies and full doc-section text
        /// by default, which bloats agent context and costs tokens.
        /// See `bench/README.md` for the measurement that motivated
        /// this flag.
        #[arg(long, value_name = "N", default_value_t = 0)]
        max_tool_result_bytes: usize,

        /// MCP tool-surface profile: `minimal` | `standard` | `full`.
        /// Filters which tools appear in `tools/list` responses to
        /// shrink the per-turn cache cost (~96 k tokens dominated by
        /// tool descriptions on Haiku; `bench/README.md`). The
        /// dispatch path is unchanged — every `tools/call` still
        /// reaches the engine regardless of profile.
        ///
        /// Falls back to `[serve] profile` in the config TOML if not
        /// passed, then to `standard`.
        #[arg(long, value_name = "PROFILE")]
        profile: Option<String>,

        /// How much of each tool's `description` to keep in the
        /// advertised `tools/list` payload. Accepts: an integer (cap
        /// at N bytes, no marker), `none` (pass through), or `all`
        /// (drop entirely — violates strict MCP spec).
        ///
        /// Falls back to `[serve] strip_tool_descriptions` in the
        /// config TOML if not passed, then to `200`.
        #[arg(long, value_name = "MODE")]
        strip_tool_descriptions: Option<String>,
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

        /// Phase 2 LLM distillation: after the heuristic pass, send
        /// each symbol (signature + body excerpt) to an OpenAI-
        /// compatible chat endpoint for a 1-sentence summary stored
        /// in `_hdb_plugin_symbol_cards.llm_summary`. Typical
        /// deployment: a self-hosted Ollama (or vLLM) endpoint
        /// reachable over Tailscale.
        #[arg(long, default_value_t = false)]
        with_llm_distill: bool,
        /// OpenAI-compatible chat endpoint (no trailing slash). Used
        /// only with `--with-llm-distill`.
        #[arg(long, default_value = "http://ollama:11434")]
        llm_distill_endpoint: String,
        /// Model tag at the endpoint.
        #[arg(long, default_value = "qwen3-coder:30b")]
        llm_distill_model: String,
        /// Number of in-flight Ollama requests.
        #[arg(long, default_value_t = 4)]
        llm_distill_concurrency: usize,
        /// Cap symbols distilled this run (0 = no cap). Useful for
        /// incremental rollout on a 100k+ symbol corpus.
        #[arg(long, default_value_t = 0)]
        llm_distill_max_symbols: usize,
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

        /// Phase 2 LLM distillation: see `Init --with-llm-distill`.
        #[arg(long, default_value_t = false)]
        with_llm_distill: bool,
        #[arg(long, default_value = "http://ollama:11434")]
        llm_distill_endpoint: String,
        #[arg(long, default_value = "qwen3-coder:30b")]
        llm_distill_model: String,
        #[arg(long, default_value_t = 4)]
        llm_distill_concurrency: usize,
        #[arg(long, default_value_t = 0)]
        llm_distill_max_symbols: usize,
    },

    /// Show config and per-KB stats. No `--source` ⇒ global summary.
    Status {
        #[arg(long)]
        source: Option<PathBuf>,

        /// If a `serve --http <addr>` is running for this source,
        /// fetch live cache stats from it. Best-effort: a dead /
        /// unreachable URL prints a one-line note but doesn't fail.
        /// The cache lives in the SERVING process; without this
        /// flag the cache stats can't be retrieved by the separate
        /// `status` invocation (the engine's `result_cache` is a
        /// per-process static).
        #[arg(long, value_name = "URL", requires = "source")]
        mcp_url: Option<String>,
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
        Commands::Serve {
            source,
            http,
            max_tool_result_bytes,
            profile,
            strip_tool_descriptions,
        } => {
            serve(
                &source,
                http.as_deref(),
                max_tool_result_bytes,
                profile.as_deref(),
                strip_tool_descriptions.as_deref(),
            )
            .await
        }
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
            with_llm_distill,
            llm_distill_endpoint,
            llm_distill_model,
            llm_distill_concurrency,
            llm_distill_max_symbols,
        } => {
            let mode = KbMode::parse(&mode)?;
            init(&source, mode, kb.as_deref())?;
            if ingest {
                let canonical_source = source.canonicalize().unwrap_or_else(|_| source.clone());
                let kb_dir = lookup_kb_dir(&canonical_source)?;
                let opts = IngestOptions {
                    source_root: canonical_source,
                    kb_dir,
                    include_binary_docs,
                    force_reparse: force,
                    durable_writes,
                    with_embeddings,
                    background_quality,
                    llm_distill: build_llm_distill_opts(
                        with_llm_distill,
                        &llm_distill_endpoint,
                        &llm_distill_model,
                        llm_distill_concurrency,
                        llm_distill_max_symbols,
                    ),
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
            with_llm_distill,
            llm_distill_endpoint,
            llm_distill_model,
            llm_distill_concurrency,
            llm_distill_max_symbols,
        } => {
            let canonical_source = source.canonicalize()?;
            let kb_dir = lookup_kb_dir(&canonical_source)?;
            let opts = IngestOptions {
                source_root: canonical_source,
                kb_dir,
                include_binary_docs,
                force_reparse: force,
                durable_writes,
                with_embeddings,
                background_quality,
                llm_distill: build_llm_distill_opts(
                    with_llm_distill,
                    &llm_distill_endpoint,
                    &llm_distill_model,
                    llm_distill_concurrency,
                    llm_distill_max_symbols,
                ),
            };
            run_and_print_ingest(&opts)
        }
        Commands::Status { source, mcp_url } => status(source.as_deref(), mcp_url.as_deref()),
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

/// Translate the four `--llm-distill-*` CLI flags into the engine
/// options struct. `None` when LLM distillation is off.
fn build_llm_distill_opts(
    enabled: bool,
    endpoint: &str,
    model: &str,
    concurrency: usize,
    max_symbols: usize,
) -> Option<distill::LlmDistillOptions> {
    if !enabled {
        return None;
    }
    Some(distill::LlmDistillOptions {
        endpoint: endpoint.to_string(),
        model: model.to_string(),
        max_tokens: 80,
        timeout_secs: 60,
        concurrency: concurrency.max(1),
        max_symbols,
    })
}

async fn serve(
    source: &std::path::Path,
    http: Option<&str>,
    max_tool_result_bytes: usize,
    cli_profile: Option<&str>,
    cli_strip: Option<&str>,
) -> Result<()> {
    let cfg = Config::load_or_default()?;
    let spec = cfg
        .lookup_for_source(source)
        .with_context(|| format!(
            "no KB configured for source `{}`. Run `heliosdb-codekb-mcp init --source {} --mode <co-located|global|hybrid>` first.",
            source.display(),
            source.display(),
        ))?;

    // Resolve gateway config: CLI flag → config TOML → built-in default.
    let profile_str = cli_profile
        .map(str::to_string)
        .or_else(|| cfg.serve.profile.clone())
        .unwrap_or_else(|| "standard".to_string());
    let strip_str = cli_strip
        .map(str::to_string)
        .or_else(|| cfg.serve.strip_tool_descriptions.clone())
        .unwrap_or_else(|| "200".to_string());
    let profile = mcp_trim::Profile::parse(&profile_str).map_err(|e| anyhow::anyhow!(e))?;
    let strip_desc = mcp_trim::StripDescMode::parse(&strip_str).map_err(|e| anyhow::anyhow!(e))?;

    tracing::info!(kb = %spec.kb_dir.display(), profile = profile.as_str(), "opening KB");
    let db = Arc::new(EmbeddedDatabase::new(&spec.kb_dir).with_context(|| {
        format!(
            "failed to open EmbeddedDatabase at {}",
            spec.kb_dir.display()
        )
    })?);

    let gateway_cfg = GatewayCfg {
        profile,
        strip_desc,
        max_tool_result_bytes,
    };

    match http {
        None => {
            // Pass-through fast path: no filtering, no shortening, no
            // result trimming → use the engine's loop unchanged.
            if gateway_cfg.is_passthrough() {
                tracing::info!("starting MCP stdio server (engine loop, no gateway rewrite)");
                let mut server = heliosdb_nano::mcp::McpServer::new(db);
                server
                    .run()
                    .await
                    .map_err(|e| anyhow::anyhow!("MCP server failed: {e}"))
            } else {
                tracing::info!(
                    profile = gateway_cfg.profile.as_str(),
                    cap = max_tool_result_bytes,
                    "starting MCP stdio server (plugin gateway loop)"
                );
                stdio_loop_with_gateway(db.as_ref(), &gateway_cfg).await
            }
        }
        Some(addr) => {
            let listener = tokio::net::TcpListener::bind(addr)
                .await
                .with_context(|| format!("bind MCP HTTP listener on {addr}"))?;
            let bound = listener
                .local_addr()
                .map(|a| a.to_string())
                .unwrap_or_else(|_| addr.to_string());
            eprintln!("MCP HTTP server listening on http://{bound}");
            eprintln!("  POST /         JSON-RPC 2.0 (plugin gateway)");
            eprintln!("  GET  /ws       WebSocket upgrade (engine, pass-through)");
            eprintln!("  GET  /sse      server-sent events (engine, pass-through)");
            eprintln!("  GET  /info     discovery + cache stats (engine, pass-through)");
            tracing::info!(
                %bound,
                profile = gateway_cfg.profile.as_str(),
                "starting MCP HTTP server"
            );
            let shutdown = async {
                let _ = tokio::signal::ctrl_c().await;
                tracing::info!("MCP HTTP server received Ctrl-C, shutting down");
            };
            let app = build_http_gateway_router(db, gateway_cfg);
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown)
                .await
                .map_err(|e| anyhow::anyhow!("MCP HTTP server failed: {e}"))
        }
    }
}

/// Per-serve gateway config. Cheap to clone (`Copy` on the inner enums,
/// plain `usize`) — fits in a tower `Extension`.
#[derive(Clone, Copy)]
struct GatewayCfg {
    profile: mcp_trim::Profile,
    strip_desc: mcp_trim::StripDescMode,
    max_tool_result_bytes: usize,
}

impl GatewayCfg {
    /// `true` when no rewrites are needed AND the plugin has no
    /// wrappers to inject — the engine's loop / router can serve
    /// directly. Layer 2 added the plugin-wrapper tools, so the
    /// passthrough path is now narrower: only `Profile::Full` with no
    /// stripping AND no body cap qualifies, AND we never expose
    /// wrappers under Full (Full means "engine surface as-is"). Since
    /// `Profile::Full` returns `true` for *every* tool name, the
    /// wrapper tools ARE allowed under it — but we still don't inject
    /// them in the passthrough branch because the engine doesn't
    /// implement their dispatch. So this check stays exactly the same;
    /// the passthrough path runs only when the user opted into
    /// engine-native behaviour.
    fn is_passthrough(&self) -> bool {
        self.profile == mcp_trim::Profile::Full
            && matches!(self.strip_desc, mcp_trim::StripDescMode::None)
            && self.max_tool_result_bytes == 0
    }
}

/// Apply the gateway's wire-level rewrites to a serialized JSON-RPC
/// response based on the original request method. Used by both
/// transports so they stay in lockstep.
///
/// Order for `tools/list`: engine → inject plugin wrappers → profile
/// filter + description strip. Injection comes BEFORE filtering so
/// the profile's allow list applies symmetrically to plugin and
/// engine tools.
fn apply_gateway_rewrite(json: &str, method: &str, cfg: &GatewayCfg) -> String {
    match method {
        "tools/list" => {
            let with_wrappers = wrappers::inject_into_tools_list(json, cfg.profile);
            mcp_trim::trim_tools_list_wire(&with_wrappers, cfg.profile, cfg.strip_desc)
        }
        "tools/call" if cfg.max_tool_result_bytes > 0 => {
            mcp_trim::trim_rpc_response_wire(json, cfg.max_tool_result_bytes)
        }
        _ => json.to_string(),
    }
}

/// Extract `params.name` from a `tools/call` request so the dispatch
/// layer can short-circuit to a plugin wrapper before the engine
/// runs. Returns `None` for missing / non-string `name`.
fn tools_call_name(params: &serde_json::Value) -> Option<&str> {
    params.get("name").and_then(|v| v.as_str())
}

/// Build a JSON-RPC `result` envelope for a successful plugin
/// dispatch, mirroring the engine's `RpcResponse::success` shape.
fn plugin_success_response(id: serde_json::Value, value: serde_json::Value) -> String {
    let envelope = wrappers::wrap_call_result(value);
    serde_json::to_string(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": envelope,
    }))
    .unwrap_or_else(|_| String::new())
}

/// Build a JSON-RPC `result` envelope marked `isError: true` for a
/// plugin handler's user-facing failure. Note: this is NOT the
/// JSON-RPC error frame (`-32xxx` codes) — MCP carries handler
/// failures inside `result.isError`, leaving the JSON-RPC error
/// frame for protocol-level problems.
fn plugin_handler_error_response(id: serde_json::Value, msg: String) -> String {
    let envelope = wrappers::wrap_call_error(msg);
    serde_json::to_string(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": envelope,
    }))
    .unwrap_or_else(|_| String::new())
}

/// Build an axum `Router<()>` that mirrors the engine's `mcp_router`
/// shape but interposes our gateway on `POST /`. The streaming
/// transports (`/ws`, `/sse`) and discovery (`/info`) are mounted
/// straight from the engine — buffering them would break the stream.
fn build_http_gateway_router(
    db: Arc<EmbeddedDatabase>,
    cfg: GatewayCfg,
) -> axum::Router {
    use axum::extract::{Extension, State};
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    use axum::Json;
    use heliosdb_nano::mcp::axum_routes::{handle_info, handle_sse, handle_ws_upgrade};
    use heliosdb_nano::mcp::rpc::{handle_rpc_with_db, RpcRequest};
    use heliosdb_nano::mcp::McpState;

    async fn gateway_post(
        State(state): State<McpState>,
        Extension(cfg): Extension<GatewayCfg>,
        Json(req): Json<RpcRequest>,
    ) -> impl IntoResponse {
        let method = req.method.clone();
        let id = req.id.clone().unwrap_or(serde_json::Value::Null);

        let out = if method == "tools/call" {
            if let Some(name) = tools_call_name(&req.params) {
                if let Some(result) = wrappers::dispatch(
                    state.db.as_ref(),
                    name,
                    req.params.get("arguments").unwrap_or(&serde_json::Value::Null),
                ) {
                    match result {
                        Ok(v) => plugin_success_response(id, v),
                        Err(msg) => plugin_handler_error_response(id, msg),
                    }
                } else {
                    let resp = handle_rpc_with_db(state.db.as_ref(), req);
                    let json = serde_json::to_string(&resp).unwrap_or_default();
                    apply_gateway_rewrite(&json, &method, &cfg)
                }
            } else {
                let resp = handle_rpc_with_db(state.db.as_ref(), req);
                let json = serde_json::to_string(&resp).unwrap_or_default();
                apply_gateway_rewrite(&json, &method, &cfg)
            }
        } else {
            let resp = handle_rpc_with_db(state.db.as_ref(), req);
            let json = serde_json::to_string(&resp).unwrap_or_default();
            apply_gateway_rewrite(&json, &method, &cfg)
        };

        (
            axum::http::StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            out,
        )
            .into_response()
    }

    let state = McpState::new(db);
    axum::Router::new()
        .route("/", post(gateway_post))
        .route("/ws", get(handle_ws_upgrade))
        .route("/sse", get(handle_sse))
        .route("/info", get(handle_info))
        .layer(Extension(cfg))
        .with_state(state)
}

/// Custom JSON-RPC stdio loop that mirrors `heliosdb_nano::mcp::McpServer::run`
/// but post-processes every response through `crate::mcp_trim` before
/// writing it to stdout. Used whenever the user has asked for any
/// gateway-level rewrite — profile filtering, description shortening,
/// or per-call result-body trimming.
///
/// Caveats vs the engine's loop:
///
/// * No streaming-progress dispatch (the engine's loop has a special
///   path for `tools/call` with `_meta.progressToken` that emits
///   `notifications/progress` mid-call). The bench workload doesn't
///   use progress tokens, and the gateway mode is opt-out (the
///   `GatewayCfg::is_passthrough()` branch in `serve()` keeps the
///   engine's loop when no rewrites are needed). If a request
///   includes a progressToken we still serve it — just synchronously,
///   with the rewrite applied to the final response (no per-chunk
///   streaming notifications).
/// * `initialized` notifications are no-ops, same as the engine.
async fn stdio_loop_with_gateway(
    db: &heliosdb_nano::EmbeddedDatabase,
    cfg: &GatewayCfg,
) -> Result<()> {
    use heliosdb_nano::mcp::rpc::{handle_rpc_with_db, RpcRequest, RpcResponse};
    use std::io::{BufRead, BufReader, Write};

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let reader = BufReader::new(stdin.lock());

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(error = %e, "stdin read failed");
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }

        let req: RpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                // Wire-level parse error — emit a JSON-RPC parse-error
                // response so the client can recover.
                let err = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": serde_json::Value::Null,
                    "error": { "code": -32700, "message": format!("Parse error: {e}") }
                });
                writeln!(stdout, "{}", err)
                    .and_then(|()| stdout.flush())
                    .map_err(|e| anyhow::anyhow!("stdout write failed: {e}"))?;
                continue;
            }
        };

        // `initialized` is a notification — no response.
        if req.method == "initialized" {
            continue;
        }

        let method = req.method.clone();
        let id = req.id.clone().unwrap_or(serde_json::Value::Null);

        // Plugin-wrapper short-circuit: if this is a tools/call for a
        // wrapper name, dispatch in-process and skip the engine.
        let out_line = if method == "tools/call" {
            if let Some(name) = tools_call_name(&req.params) {
                if let Some(result) = wrappers::dispatch(db, name, req.params.get("arguments").unwrap_or(&serde_json::Value::Null)) {
                    match result {
                        Ok(v) => plugin_success_response(id, v),
                        Err(msg) => plugin_handler_error_response(id, msg),
                    }
                } else {
                    let resp: RpcResponse = handle_rpc_with_db(db, req);
                    let json = serde_json::to_string(&resp).unwrap_or_default();
                    apply_gateway_rewrite(&json, &method, cfg)
                }
            } else {
                let resp: RpcResponse = handle_rpc_with_db(db, req);
                let json = serde_json::to_string(&resp).unwrap_or_default();
                apply_gateway_rewrite(&json, &method, cfg)
            }
        } else {
            let resp: RpcResponse = handle_rpc_with_db(db, req);
            let json = match serde_json::to_string(&resp) {
                Ok(j) => j,
                Err(e) => {
                    tracing::error!(error = %e, "response serialize failed");
                    continue;
                }
            };
            apply_gateway_rewrite(&json, &method, cfg)
        };

        writeln!(stdout, "{}", out_line)
            .and_then(|()| stdout.flush())
            .map_err(|e| anyhow::anyhow!("stdout write failed: {e}"))?;
    }
    Ok(())
}

fn init(
    source: &std::path::Path,
    mode: KbMode,
    kb_override: Option<&std::path::Path>,
) -> Result<()> {
    let source = source.canonicalize().with_context(|| {
        format!(
            "source path `{}` must exist and be canonicalisable",
            source.display()
        )
    })?;
    let spec = KbSpec::resolve(&source, mode, kb_override)?;

    std::fs::create_dir_all(&spec.kb_dir)
        .with_context(|| format!("failed to create KB directory {}", spec.kb_dir.display()))?;

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

fn status(source: Option<&std::path::Path>, mcp_url: Option<&str>) -> Result<()> {
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
                print_resume_state(&spec.kb_dir);
                print_quality_phase(&spec.kb_dir);
                if let Some(url) = mcp_url {
                    print_mcp_cache_stats(url);
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
            println!(
                "  {}  →  {}  ({})",
                src,
                spec.kb_dir.display(),
                spec.mode.as_str()
            );
        }
    }
    Ok(())
}

/// Best-effort fetch + render of `cache` from the running MCP
/// server's `/info` endpoint.  Cache state is per-process (the
/// engine's `result_cache` is a `static`), so the only way for
/// `status` to see it is to talk to the live server.  A failure
/// is downgraded to a one-line note — never an exit code.
fn print_mcp_cache_stats(url: &str) {
    let info_url = if url.trim_end_matches('/').ends_with("/info") {
        url.to_string()
    } else {
        format!("{}/info", url.trim_end_matches('/'))
    };
    let resp = match ureq::get(&info_url)
        .timeout(std::time::Duration::from_millis(750))
        .call()
    {
        Ok(r) => r,
        Err(e) => {
            println!("mcp cache : (could not reach {info_url}: {e})");
            return;
        }
    };
    let info: serde_json::Value = match resp.into_json() {
        Ok(v) => v,
        Err(e) => {
            println!("mcp cache : (response from {info_url} was not JSON: {e})");
            return;
        }
    };
    let cache = match info.get("cache") {
        Some(c) if c.is_object() => c,
        _ => {
            println!("mcp cache : (server at {info_url} did not include `cache` field)");
            return;
        }
    };
    let size = cache.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
    let cap = cache.get("capacity").and_then(|v| v.as_u64()).unwrap_or(0);
    let gen_n = cache
        .get("generation")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let hits = cache.get("hits").and_then(|v| v.as_u64()).unwrap_or(0);
    let misses = cache.get("misses").and_then(|v| v.as_u64()).unwrap_or(0);
    let hit_rate = cache
        .get("hit_rate")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    println!(
        "mcp cache : {size} / {cap} entries, {:.1}% hit rate ({hits} hit / {misses} miss), gen {gen_n}",
        hit_rate * 100.0,
    );
}

/// Pretty-print the ingest resume state (`.ingest-state.json`).
/// Silent when no checkpoint is present (the steady-state condition
/// — `ingest` clears the file on success).
fn print_resume_state(kb_dir: &std::path::Path) {
    let cp = match checkpoint::read(kb_dir) {
        Ok(Some(cp)) => cp,
        Ok(None) => return,
        Err(e) => {
            println!("ingest resume : (error reading checkpoint: {e})");
            return;
        }
    };
    let now = quality::now_secs();
    let elapsed = now.saturating_sub(cp.started_at_secs);
    println!(
        "ingest resume : interrupted at phase = {:?} ({} ago) — re-run `ingest` to continue",
        cp.phase,
        quality::fmt_duration_secs(elapsed),
    );
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
                println!(
                    "              : tail {} or re-run `ingest --background-quality`",
                    p.log_path
                );
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

/// Resolve the canonical KB directory for a source root via the
/// user's config TOML. Errors with a friendly hint if no KB has
/// been registered for the source.
fn lookup_kb_dir(canonical_source: &std::path::Path) -> Result<PathBuf> {
    let cfg = Config::load_or_default()?;
    let spec = cfg.lookup_for_source(canonical_source).with_context(|| {
        format!(
            "no KB configured for source `{}`. Run `heliosdb-codekb-mcp init --source {} --mode <co-located|global|hybrid>` first.",
            canonical_source.display(),
            canonical_source.display(),
        )
    })?;
    Ok(spec.kb_dir.clone())
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
        if opts.durable_writes {
            "Sync (durable)"
        } else {
            "Async (fast)"
        }
    );
    eprintln!("  files seen    : {}", summary.files_seen);
    eprintln!(
        "  upserted      : {} code, {} text, {} markdown, {} binary-doc",
        summary.code_upserts, summary.doc_upserts, summary.md_doc_upserts, summary.binary_upserts
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
            c.parse_elapsed_ms, c.write_elapsed_ms, c.parse_workers, c.chunks_processed
        );
    }
    if let Some(d) = summary.docs {
        eprintln!(
            "  graph_rag row : nodes={} edges={} rows_seen={} rows_skipped={}",
            d.nodes_added, d.edges_added, d.rows_seen, d.rows_skipped
        );
    }
    if let Some(d) = summary.docs_md {
        eprintln!(
            "  graph_rag md  : nodes={} edges={} rows_seen={} rows_skipped={}  (heading-chunked DocSection + PART_OF)",
            d.nodes_added, d.edges_added, d.rows_seen, d.rows_skipped
        );
    }
    if let Some(l) = summary.links {
        eprintln!(
            "  linker        : nodes_scanned={} mentions_added={} candidates={}",
            l.nodes_scanned, l.mentions_added, l.candidates_seen
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
        .arg("--source")
        .arg(source_root)
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
    eprintln!(
        "  heliosdb-codekb-mcp status --source {}",
        source_root.display()
    );
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
    if body
        .lines()
        .any(|l| l.trim() == entry.trim_end_matches('/') || l.trim() == entry)
    {
        return Ok(());
    }
    let mut new = body;
    if !new.is_empty() && !new.ends_with('\n') {
        new.push('\n');
    }
    new.push_str(entry);
    new.push('\n');
    std::fs::write(&path, new).with_context(|| format!("failed to update {}", path.display()))?;
    Ok(())
}
