//! Layer 3 — heuristic pre-distillation at ingest.
//!
//! Populates two plugin-owned tables that back the wrappers in
//! [`crate::wrappers`]:
//!
//! * `_hdb_plugin_symbol_cards` — per-symbol distilled card:
//!   signature + first-line docstring + a content hash for
//!   idempotency. Used by `helios_symbol_card` for the docstring
//!   field (LSP `hover` is the fallback for ad-hoc symbols).
//! * `_hdb_plugin_repomap_cards` — per-file Aider-style RepoMap row:
//!   PageRank score over the symbol call graph, projected to file
//!   level, plus a small JSON array of the file's top public
//!   symbols (name + signature). Used by `helios_repo_summary`.
//!
//! Algorithm summary:
//!
//! 1. **Symbol cards**: stream `_hdb_code_symbols` + the source file
//!    content from the plugin's `src` table, extract one heuristic
//!    "doc1l" line from the comment immediately preceding `line_start`,
//!    UPSERT a row keyed on `qualified`. Skip when the content_hash
//!    matches the existing row (idempotent re-run).
//! 2. **PageRank**: power-method iteration over the CALLS edges in
//!    `_hdb_code_symbol_refs` (kind = 'CALLS'), damping `d=0.85`,
//!    cap 50 iterations or L1-Δ < 1e-6. Symbol scores get projected
//!    to file-level by summing the scores of symbols in each file.
//! 3. **RepoMap cards**: write one row per file with the file-level
//!    PageRank and the top N symbols by signature length (proxy for
//!    public surface area). Layer 4 follow-up could weight by symbol
//!    PageRank instead.
//!
//! Phase 1 is heuristic-only — no LLM, no external service. Phase 2
//! adds an opt-in `--with-llm-distill` flag that calls a local model
//! for a 1-sentence purpose summary; see plan docs.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Write};

use anyhow::{Context, Result};
use heliosdb_nano::{EmbeddedDatabase, Value};

// Bulk-write tuning, mirroring `crate::linker`'s constants. Picked so
// that a single statement fits in ~40 KB of SQL text (well under any
// reasonable parser limit) and a single batch commits ~25 K rows
// (fits in the engine's default WAL flush window without stalling).
const ROWS_PER_INSERT_STMT: usize = 500;
const STMTS_PER_BATCH: usize = 50;
// Hard cap on per-row text fields. Distill cards are advisory; we'd
// rather drop a few KB of a giant function signature than blow up
// the SQL text size.
const MAX_FIELD_BYTES: usize = 4096;

#[derive(Debug, Default, Clone)]
pub struct DistillStats {
    pub symbols_scanned: usize,
    pub symbols_written: usize,
    pub symbols_unchanged: usize,
    pub files_written: usize,
    pub pagerank_iters: u32,
    pub pagerank_converged: bool,
}

const SYMBOL_CARDS_TABLE: &str = "_hdb_plugin_symbol_cards";
const REPOMAP_CARDS_TABLE: &str = "_hdb_plugin_repomap_cards";

// ---------------------------------------------------------------------------
// DDL — kept idempotent so re-running doesn't break on existing schema.
// ---------------------------------------------------------------------------

fn ensure_tables(db: &EmbeddedDatabase) -> Result<()> {
    db.execute(&format!(
        "CREATE TABLE IF NOT EXISTS {SYMBOL_CARDS_TABLE} (
            qualified    TEXT PRIMARY KEY,
            signature    TEXT,
            doc1l        TEXT,
            content_hash TEXT
         )"
    ))
    .with_context(|| format!("create {SYMBOL_CARDS_TABLE}"))?;
    // Phase 2 column — best-effort add; engine ALTER TABLE IF NOT
    // EXISTS keeps re-runs cheap and lets the heuristic-only path
    // co-exist with the LLM-distill path. Older binaries see the
    // column as NULL and ignore it.
    let _ = db.execute(&format!(
        "ALTER TABLE {SYMBOL_CARDS_TABLE} ADD COLUMN IF NOT EXISTS llm_summary TEXT"
    ));
    let _ = db.execute(&format!(
        "ALTER TABLE {SYMBOL_CARDS_TABLE} ADD COLUMN IF NOT EXISTS llm_model TEXT"
    ));
    db.execute(&format!(
        "CREATE TABLE IF NOT EXISTS {REPOMAP_CARDS_TABLE} (
            path         TEXT PRIMARY KEY,
            pagerank     DOUBLE PRECISION,
            top_symbols  TEXT
         )"
    ))
    .with_context(|| format!("create {REPOMAP_CARDS_TABLE}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Symbol cards — signature + first-line docstring + content_hash
// ---------------------------------------------------------------------------

/// Walk `_hdb_code_symbols`, derive a `doc1l` heuristic per symbol
/// from the source file's content, UPSERT one row per `qualified`
/// in `_hdb_plugin_symbol_cards`. Idempotent: matching `content_hash`
/// rows are left untouched.
pub fn build_symbol_cards(db: &EmbeddedDatabase) -> Result<DistillStats> {
    ensure_tables(db)?;
    let mut stats = DistillStats::default();

    // Pull existing cards for idempotency comparison. NOTE: the
    // engine's `query` path emits a parser warning + returns Err on
    // tables created mid-session in some scenarios; we tolerate that
    // and fall back to an empty map (a re-run then re-INSERTs every
    // row, hitting the PK guard — which is then handled by always
    // pairing the INSERTs with a DELETE-by-key in the bulk_upsert
    // path below).
    let existing: HashMap<String, String> = match db.query(
        &format!("SELECT qualified, content_hash FROM {SYMBOL_CARDS_TABLE}"),
        &[],
    ) {
        Ok(rows) => rows
            .iter()
            .filter_map(|r| {
                let q = tuple_str(r, 0);
                let h = tuple_str(r, 1);
                if q.is_empty() {
                    None
                } else {
                    Some((q, h))
                }
            })
            .collect(),
        Err(e) => {
            tracing::debug!("symbol_cards existing-load returned err (treating as empty): {e}");
            HashMap::new()
        }
    };
    tracing::debug!(
        "build_symbol_cards: existing.len()={}",
        existing.len()
    );

    // Load every src file's content once, build a per-file line index
    // keyed by path so the per-symbol comment lookup is O(1).
    let mut file_lines: HashMap<String, Vec<String>> = HashMap::new();
    if let Ok(rows) = db.query("SELECT path, content FROM src", &[]) {
        for row in &rows {
            let path = tuple_str(row, 0);
            let content = tuple_str(row, 1);
            if path.is_empty() {
                continue;
            }
            let lines: Vec<String> = content.lines().map(str::to_string).collect();
            file_lines.insert(path, lines);
        }
    }

    // Engine quirk: on the FIRST ingest pass, a SELECT with a JOIN
    // against `_hdb_code_symbols` returns 0 rows (observed in
    // bench/codekb-smoke runs; the same JOIN in build_repomap_cards
    // works because it runs after this function). Pull the two
    // tables separately and join in Rust — slower in theory but
    // reliable and tolerable at the corpus scales we care about.
    let sym_rows = db
        .query(
            "SELECT node_id, file_id, qualified, signature, line_start FROM _hdb_code_symbols",
            &[],
        )
        .context("fetch _hdb_code_symbols")?;
    let file_rows = db
        .query("SELECT node_id, path FROM _hdb_code_files", &[])
        .context("fetch _hdb_code_files")?;
    let file_path: HashMap<i64, String> = file_rows
        .iter()
        .filter_map(|r| {
            let id = tuple_int(r, 0)?;
            let p = tuple_str(r, 1);
            if p.is_empty() { None } else { Some((id, p)) }
        })
        .collect();
    stats.symbols_scanned = sym_rows.len();

    // Single-pass bulk write. Hash-gate filters unchanged symbols
    // before we ever touch the disk; the surviving (new + changed)
    // rows are streamed to a tempfile as one-INSERT-per-row batches
    // applied via `execute_batch` under `SET bulk_load_mode = true` —
    // same pattern as `crate::linker::link_mentions_bulk` (the engine
    // supports multi-row VALUES only for engine-managed tables like
    // `_hdb_graph_edges`). For changed rows we DELETE first since
    // `INSERT … ON CONFLICT` isn't on the engine's `execute_batch`
    // path (only `execute_params` supports it).
    //
    // `_hdb_code_symbols.qualified` is NOT unique (a struct + an impl
    // method can share a qualified prefix in some grammars; impls with
    // the same name appear once per inherent block). Dedup last-wins
    // via a HashMap before emitting INSERTs so the PK constraint on
    // `_hdb_plugin_symbol_cards.qualified` doesn't fire.
    let mut staged: HashMap<String, (String, String, String)> = HashMap::new();
    for row in &sym_rows {
        let _node_id = tuple_int(row, 0);
        let file_id = match tuple_int(row, 1) {
            Some(id) => id,
            None => continue,
        };
        let qualified = tuple_str(row, 2);
        let signature = tuple_str(row, 3);
        let line_start = tuple_int(row, 4).unwrap_or(0) as usize;
        if qualified.is_empty() {
            continue;
        }
        let path = match file_path.get(&file_id) {
            Some(p) => p.clone(),
            None => continue,
        };
        let doc1l = file_lines
            .get(&path)
            .map(|lines| extract_doc1l(lines, line_start))
            .unwrap_or_default();
        let hash = blake_hex(&format!("{signature}\n{doc1l}"));
        if existing.get(&qualified).map(|h| h == &hash).unwrap_or(false) {
            stats.symbols_unchanged += 1;
            continue;
        }
        staged.insert(qualified, (cap(signature), cap(doc1l), hash));
    }

    if staged.is_empty() {
        return Ok(stats);
    }

    // Always DELETE the keys we're about to INSERT — covers the case
    // where `existing` was loaded as empty (engine query glitch on a
    // mid-session created table) while the table is in fact populated.
    // Cheaper than the PK conflict that would otherwise abort the batch.
    let to_delete: Vec<String> = staged.keys().cloned().collect();
    let to_write: Vec<(String, String, String, String)> = staged
        .into_iter()
        .map(|(q, (s, d, h))| (q, s, d, h))
        .collect();
    bulk_upsert(
        db,
        SYMBOL_CARDS_TABLE,
        "qualified",
        &["qualified", "signature", "doc1l", "content_hash"],
        &to_delete,
        to_write
            .iter()
            .map(|(q, s, d, h)| {
                vec![
                    (q.as_str(), false),
                    (s.as_str(), false),
                    (d.as_str(), false),
                    (h.as_str(), false),
                ]
            })
            .collect::<Vec<_>>()
            .as_slice(),
    )?;
    stats.symbols_written = to_write.len();
    Ok(stats)
}

/// Bulk DELETE (by key column) + INSERT (one row per statement) via
/// `execute_batch` under `SET bulk_load_mode = true`. Same pattern as
/// `crate::linker::link_mentions_bulk`. `key_col` is the PK column we
/// DELETE on for changed rows; `cols` is the column list for the
/// INSERT (`key_col` must be the first entry). Each entry in `rows`
/// is a slice of (value, is_numeric) — when `is_numeric` we emit the
/// value unquoted, otherwise `sql_lit`-escaped.
fn bulk_upsert(
    db: &EmbeddedDatabase,
    table: &str,
    key_col: &str,
    cols: &[&str],
    delete_keys: &[String],
    rows: &[Vec<(&str, bool)>],
) -> Result<()> {
    if rows.is_empty() && delete_keys.is_empty() {
        return Ok(());
    }
    let tmp = tempfile::NamedTempFile::new().context("create distill tempfile")?;
    let mut writer = BufWriter::new(tmp.reopen().context("reopen distill tempfile")?);

    // Phase A: DELETEs in chunks of ROWS_PER_INSERT_STMT keys.
    for chunk in delete_keys.chunks(ROWS_PER_INSERT_STMT) {
        let mut stmt = format!("DELETE FROM {table} WHERE {key_col} IN (");
        for (i, k) in chunk.iter().enumerate() {
            if i > 0 {
                stmt.push(',');
            }
            stmt.push_str(&sql_lit(k));
        }
        stmt.push(')');
        writeln!(writer, "{stmt}").context("write delete stmt")?;
    }

    // Phase B: one INSERT statement per row. The engine's `execute`
    // / `execute_batch` paths return "Operator not yet implemented:
    // Insert with values: [[…], […]]" when more than one VALUES
    // tuple is supplied for a user-defined table — the linker only
    // gets away with multi-row INSERTs into `_hdb_graph_edges` (an
    // engine-managed table). Single-row INSERTs are fine. Batching
    // 50 statements per execute_batch still amortises ~all of the
    // per-call cost.
    let cols_csv = cols.join(", ");
    for row in rows {
        let mut stmt = format!("INSERT INTO {table} ({cols_csv}) VALUES (");
        for (j, (v, is_numeric)) in row.iter().enumerate() {
            if j > 0 {
                stmt.push(',');
            }
            if *is_numeric {
                stmt.push_str(v);
            } else {
                stmt.push_str(&sql_lit(v));
            }
        }
        stmt.push(')');
        writeln!(writer, "{stmt}").context("write insert stmt")?;
    }
    writer.flush().context("flush distill tempfile")?;
    drop(writer);

    // Apply pass — best-effort SET bulk_load_mode, then batched
    // execute_batch. Older engine versions don't recognise the
    // setting; we still proceed in either case.
    let bulk_enabled = db.execute("SET bulk_load_mode = true").is_ok();
    let result = apply_from_tempfile(db, tmp.path());
    if bulk_enabled {
        let _ = db.execute("SET bulk_load_mode = false");
    }
    result
}

fn apply_from_tempfile(db: &EmbeddedDatabase, path: &std::path::Path) -> Result<()> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("re-open distill tempfile {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut batch: Vec<String> = Vec::with_capacity(STMTS_PER_BATCH);
    for line in reader.lines() {
        let stmt = line.context("read distill tempfile line")?;
        if stmt.is_empty() {
            continue;
        }
        batch.push(stmt);
        if batch.len() >= STMTS_PER_BATCH {
            flush_batch(db, &batch)?;
            batch.clear();
        }
    }
    flush_batch(db, &batch)?;
    Ok(())
}

fn flush_batch(db: &EmbeddedDatabase, batch: &[String]) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let refs: Vec<&str> = batch.iter().map(String::as_str).collect();
    if let Err(e) = db.execute_batch(&refs) {
        // Best-effort: dump the failing batch to /tmp so the error
        // chain points at the exact statement. Helps debug parser
        // errors on long string IN-lists / multibyte literals.
        let dump = format!("/tmp/distill-failing-batch-{}.sql", std::process::id());
        let _ = std::fs::write(&dump, batch.join("\n"));
        anyhow::bail!(
            "execute_batch({} stmts) for distill bulk load: {e:#} (failing batch dumped to {dump})",
            refs.len()
        );
    }
    Ok(())
}

/// ASCII-sanitize multibyte chars in a text field. The engine's SQL
/// parser miscounts column positions on multibyte literals (em-dash
/// triggers `Unterminated string literal` reports + a related
/// `with_context.rs:125` byte-boundary panic on the parameterized
/// path). Replacing `—` with `-` etc. costs us nothing semantically;
/// the cards are advisory text.
fn sanitize(s: String) -> String {
    if s.is_ascii() {
        return s;
    }
    s.chars()
        .map(|c| if c.is_ascii() { c } else { ascii_fallback(c) })
        .collect()
}

/// Cap a text field at MAX_FIELD_BYTES on a char boundary AND
/// ASCII-sanitize. Used for fields whose worst-case size is
/// unbounded (signatures, doc1ls). Sequencing matters: sanitize
/// FIRST (replaces each multibyte char with 1 ASCII byte), THEN cap
/// — otherwise capping a mid-emoji byte position panics.
fn cap(s: String) -> String {
    let sanitized = sanitize(s);
    if sanitized.len() <= MAX_FIELD_BYTES {
        return sanitized;
    }
    sanitized[..MAX_FIELD_BYTES].to_string()
}

/// Best-effort one-char substitutes for the multibyte chars most
/// commonly seen in doc headings and code comments. Everything else
/// collapses to `?` — the card just loses a glyph, the structure is
/// preserved.
fn ascii_fallback(c: char) -> char {
    match c {
        '\u{2013}' | '\u{2014}' => '-',                 // en-dash, em-dash
        '\u{2018}' | '\u{2019}' => '\'',                // smart single quotes
        '\u{201C}' | '\u{201D}' => '"',                 // smart double quotes
        '\u{2026}' => '.',                              // ellipsis
        '\u{00A0}' => ' ',                              // nbsp
        '\u{2192}' | '\u{2190}' => '-',                 // arrows
        '\u{2713}' | '\u{2705}' => '+',                 // checkmark
        '\u{2717}' | '\u{274C}' => 'x',                 // x-mark
        _ => '?',
    }
}

/// Pull up to 120 chars of the leading comment line(s) directly above
/// `line_start`. Recognises `///`, `//`, `#`, and `"""` prefixes —
/// covers Rust, Go, TypeScript, Python, shell, and most config syntax.
///
/// `line_start` is 1-indexed (matches `_hdb_code_symbols.line_start`).
fn extract_doc1l(lines: &[String], line_start: usize) -> String {
    if line_start == 0 || line_start > lines.len() {
        return String::new();
    }
    // `line_start` is 1-indexed; the symbol's own line is at
    // `lines[line_start - 1]`. The comment we want is the one
    // immediately above.
    let mut idx = line_start.saturating_sub(2);
    // Walk backwards until we find a comment-prefixed line or a
    // non-comment line. Take only the *first* comment line above
    // (closest to the symbol).
    while idx > 0 && lines[idx].trim().is_empty() {
        idx -= 1;
    }
    if idx >= lines.len() {
        return String::new();
    }
    let candidate = lines[idx].trim();
    let body = if let Some(rest) = candidate.strip_prefix("///") {
        rest.trim()
    } else if let Some(rest) = candidate.strip_prefix("//") {
        rest.trim()
    } else if let Some(rest) = candidate.strip_prefix("#") {
        rest.trim()
    } else if let Some(rest) = candidate.strip_prefix("\"\"\"") {
        rest.trim_end_matches("\"\"\"").trim()
    } else {
        return String::new();
    };
    let cap: String = body.chars().take(120).collect();
    cap
}

// ---------------------------------------------------------------------------
// RepoMap cards — PageRank over CALLS edges projected to files
// ---------------------------------------------------------------------------

/// Compute PageRank over the symbol-level call graph, project to
/// file-level, and UPSERT one row per file with the rank + a small
/// JSON array of top symbols. Returns stats including convergence.
pub fn build_repomap_cards(db: &EmbeddedDatabase) -> Result<DistillStats> {
    ensure_tables(db)?;
    let mut stats = DistillStats::default();

    // Build the symbol → file map and a flat list of all symbol ids.
    let sym_rows = db
        .query(
            "SELECT s.node_id, f.path FROM _hdb_code_symbols s \
             JOIN _hdb_code_files f ON f.node_id = s.file_id",
            &[],
        )
        .context("fetch symbol → file map")?;

    let mut sym_to_file: HashMap<i64, String> = HashMap::with_capacity(sym_rows.len());
    let mut all_syms: Vec<i64> = Vec::with_capacity(sym_rows.len());
    for row in &sym_rows {
        let sid = tuple_int(row, 0);
        let path = tuple_str(row, 1);
        if let Some(sid) = sid {
            sym_to_file.insert(sid, path);
            all_syms.push(sid);
        }
    }
    if all_syms.is_empty() {
        return Ok(stats);
    }

    // Adjacency list: from_symbol → Vec<to_symbol>. Filter to
    // resolved CALLS edges only (unresolved have NULL to_symbol).
    let edge_rows = db
        .query(
            "SELECT from_symbol, to_symbol FROM _hdb_code_symbol_refs \
             WHERE kind = 'CALLS' AND to_symbol IS NOT NULL",
            &[],
        )
        .context("fetch CALLS edges")?;
    let mut out_edges: HashMap<i64, Vec<i64>> = HashMap::new();
    for row in &edge_rows {
        let from = match tuple_int(row, 0) {
            Some(v) => v,
            None => continue,
        };
        let to = match tuple_int(row, 1) {
            Some(v) => v,
            None => continue,
        };
        out_edges.entry(from).or_default().push(to);
    }

    let (ranks, iters, converged) = pagerank(&all_syms, &out_edges, 50, 0.85, 1e-6);
    stats.pagerank_iters = iters;
    stats.pagerank_converged = converged;

    // Project symbol scores onto files.
    let mut file_score: HashMap<String, f64> = HashMap::new();
    let mut file_syms: HashMap<String, Vec<i64>> = HashMap::new();
    for sid in &all_syms {
        let path = match sym_to_file.get(sid) {
            Some(p) => p.clone(),
            None => continue,
        };
        let score = ranks.get(sid).copied().unwrap_or(0.0);
        *file_score.entry(path.clone()).or_default() += score;
        file_syms.entry(path).or_default().push(*sid);
    }

    // Pull (name, signature) for every symbol once so we can build
    // per-file top-symbol arrays without N round-trips.
    let mut sym_card: HashMap<i64, (String, String)> = HashMap::new();
    if let Ok(rows) = db.query(
        "SELECT node_id, name, signature FROM _hdb_code_symbols",
        &[],
    ) {
        for row in &rows {
            if let Some(sid) = tuple_int(row, 0) {
                let name = tuple_str(row, 1);
                let sig = tuple_str(row, 2);
                sym_card.insert(sid, (name, sig));
            }
        }
    }

    // Pull doc1l from symbol_cards (already built by build_symbol_cards
    // if it ran earlier this pass) so the symbol_index detail mode has
    // a sentence per symbol.
    let mut sym_doc: HashMap<String, String> = HashMap::new();
    if let Ok(rows) = db.query(
        &format!("SELECT qualified, doc1l FROM {SYMBOL_CARDS_TABLE}"),
        &[],
    ) {
        for row in &rows {
            let q = tuple_str(row, 0);
            let d = tuple_str(row, 1);
            if !q.is_empty() {
                sym_doc.insert(q, d);
            }
        }
    }

    // Build owned row strings so bulk_upsert's &str slice can borrow.
    let mut rendered: Vec<(String, String, String)> = Vec::with_capacity(file_score.len());
    for (path, score) in &file_score {
        let mut syms = file_syms.remove(path).unwrap_or_default();
        syms.sort_by(|a, b| {
            ranks
                .get(b)
                .copied()
                .unwrap_or(0.0)
                .partial_cmp(&ranks.get(a).copied().unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let top_n = syms.into_iter().take(10).collect::<Vec<_>>();
        let top_json = serde_json::Value::Array(
            top_n
                .iter()
                .map(|sid| {
                    let (name, signature) = sym_card
                        .get(sid)
                        .cloned()
                        .unwrap_or((String::new(), String::new()));
                    let doc1l = sym_doc.get(&name).cloned().unwrap_or_default();
                    serde_json::json!({
                        "name": name,
                        "signature": signature,
                        "doc1l": doc1l,
                    })
                })
                .collect(),
        );
        // Top-symbols JSON is bounded (top-10 × ~500 B) so we sanitize
        // without capping — capping would shred the JSON structure.
        rendered.push((sanitize(path.clone()), format!("{score}"), sanitize(top_json.to_string())));
    }

    if !rendered.is_empty() {
        // For files we always replace — they're cheap and PageRank
        // shifts every ingest. Mass-DELETE the whole set then INSERT.
        let delete_keys: Vec<String> = rendered.iter().map(|(p, _, _)| p.clone()).collect();
        let rows: Vec<Vec<(&str, bool)>> = rendered
            .iter()
            .map(|(p, s, j)| {
                vec![
                    (p.as_str(), false),
                    (s.as_str(), true), // pagerank — emit unquoted (DOUBLE PRECISION column)
                    (j.as_str(), false),
                ]
            })
            .collect();
        bulk_upsert(
            db,
            REPOMAP_CARDS_TABLE,
            "path",
            &["path", "pagerank", "top_symbols"],
            &delete_keys,
            &rows,
        )?;
        stats.files_written = rendered.len();
    }

    Ok(stats)
}

// ---------------------------------------------------------------------------
// PageRank — power method
// ---------------------------------------------------------------------------

/// Iterative PageRank. Returns `(scores, iterations_used, converged)`.
///
/// `nodes`: list of every node id in the graph (used to seed the
/// uniform initial distribution AND to apply the teleport term to
/// sinks). `out_edges`: from-node → list of to-nodes for one edge type
/// (CALLS in this module's call site, but reusable). `max_iters`:
/// hard cap. `damping`: standard 0.85. `tol`: L1 convergence
/// threshold.
fn pagerank(
    nodes: &[i64],
    out_edges: &HashMap<i64, Vec<i64>>,
    max_iters: u32,
    damping: f64,
    tol: f64,
) -> (HashMap<i64, f64>, u32, bool) {
    let n = nodes.len() as f64;
    if n == 0.0 {
        return (HashMap::new(), 0, true);
    }
    let init = 1.0 / n;
    let mut rank: HashMap<i64, f64> = nodes.iter().map(|&v| (v, init)).collect();
    let mut next: HashMap<i64, f64> = HashMap::with_capacity(nodes.len());

    for iter in 1..=max_iters {
        next.clear();
        // Teleport baseline: every node gets (1-d)/N.
        let teleport = (1.0 - damping) / n;
        for &v in nodes {
            next.insert(v, teleport);
        }
        // Distribute current rank along out-edges; sinks contribute
        // their whole share via the teleport mass (added in bulk).
        let mut sink_mass = 0.0;
        for &v in nodes {
            let r = rank.get(&v).copied().unwrap_or(init);
            match out_edges.get(&v) {
                Some(targets) if !targets.is_empty() => {
                    let share = damping * r / targets.len() as f64;
                    for &t in targets {
                        *next.entry(t).or_default() += share;
                    }
                }
                _ => {
                    sink_mass += damping * r;
                }
            }
        }
        let sink_per = sink_mass / n;
        for v in next.values_mut() {
            *v += sink_per;
        }
        // L1 delta.
        let mut delta = 0.0;
        for &v in nodes {
            let a = rank.get(&v).copied().unwrap_or(init);
            let b = next.get(&v).copied().unwrap_or(init);
            delta += (a - b).abs();
        }
        std::mem::swap(&mut rank, &mut next);
        if delta < tol {
            return (rank, iter, true);
        }
    }
    (rank, max_iters, false)
}

// ---------------------------------------------------------------------------
// Helpers — Tuple field extraction + tiny hashing
// ---------------------------------------------------------------------------

fn tuple_str(row: &heliosdb_nano::Tuple, idx: usize) -> String {
    match row.get(idx) {
        Some(heliosdb_nano::Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

fn tuple_int(row: &heliosdb_nano::Tuple, idx: usize) -> Option<i64> {
    match row.get(idx) {
        Some(heliosdb_nano::Value::Int8(i)) => Some(*i),
        Some(heliosdb_nano::Value::Int4(i)) => Some(*i as i64),
        Some(heliosdb_nano::Value::Int2(i)) => Some(*i as i64),
        _ => None,
    }
}

fn sql_lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Hex-encoded short stable hash of the input. Uses a tiny FNV-1a
/// implementation — we don't ship blake3 as a direct dep, and the
/// hash is for idempotency comparison only (not cryptographic).
fn blake_hex(input: &str) -> String {
    // FNV-1a 64-bit, then hex.
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in input.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

// ---------------------------------------------------------------------------
// Phase 2 — LLM-distilled symbol summaries (opt-in)
// ---------------------------------------------------------------------------

/// Configuration for the LLM-distill pass. Hosted by an OpenAI-compatible
/// `/v1/chat/completions` endpoint — typically a self-hosted Ollama
/// instance reachable over Tailscale (the original deployment target).
#[derive(Debug, Clone)]
pub struct LlmDistillOptions {
    /// Base URL with no trailing slash, e.g. `http://ollama:11434`.
    pub endpoint: String,
    /// Model tag, e.g. `qwen3-coder:30b`.
    pub model: String,
    /// Hard cap on summary tokens. Most code symbols summarise in ≤40
    /// tokens; 80 leaves slack for verbose Qwen3 phrasings without
    /// runaway costs.
    pub max_tokens: u32,
    /// Optional per-call wall-time budget (seconds). 0 disables.
    pub timeout_secs: u64,
    /// Number of in-flight requests. 1 = serial; 4-8 typical for a
    /// 30B model behind a single GPU. Above 8 starts queuing.
    pub concurrency: usize,
    /// Hard cap on symbols to distill in one pass. 0 = no cap.
    /// Useful for incremental rollout on a large corpus where you
    /// want to ship the first 5k summaries today and the rest later.
    pub max_symbols: usize,
}

impl Default for LlmDistillOptions {
    fn default() -> Self {
        Self {
            endpoint: "http://ollama:11434".to_string(),
            model: "qwen3-coder:30b".to_string(),
            max_tokens: 80,
            timeout_secs: 60,
            concurrency: 4,
            max_symbols: 0,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct LlmDistillStats {
    pub candidates: usize,
    pub written: usize,
    pub unchanged: usize,
    pub failed: usize,
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
}

/// Walk `_hdb_plugin_symbol_cards` rows that don't yet have an
/// `llm_summary` matching the current `(model, content_hash)` pair,
/// POST each one to the configured Ollama endpoint, store the
/// 1-sentence response back into the row. Skips rows whose existing
/// summary was generated by the SAME model + still-valid content_hash.
pub fn build_llm_summaries(
    db: &EmbeddedDatabase,
    opts: &LlmDistillOptions,
) -> Result<LlmDistillStats> {
    use std::sync::{Arc, Mutex};
    use std::thread;

    let mut stats = LlmDistillStats::default();

    // Snapshot every symbol card. We pull from THREE tables
    // separately and join in Rust — the engine's planner intermittently
    // returns 0 rows on multi-way JOINs against fresh-txn snapshots
    // (see the note in build_symbol_cards), and ordering by a JOIN
    // column makes the issue worse.
    //
    // Symbols are sorted by their containing file's PageRank
    // (descending) so `--llm-distill-max-symbols N` covers the most
    // important code first — a 2k-cap on a 178k-symbol corpus then
    // distills the API surface, not random utility functions.
    let card_rows = db
        .query(
            "SELECT qualified, signature, doc1l, content_hash, llm_summary, llm_model \
             FROM _hdb_plugin_symbol_cards",
            &[],
        )
        .context("fetch _hdb_plugin_symbol_cards")?;
    let sym_rows = db
        .query(
            "SELECT qualified, file_id, line_start, line_end FROM _hdb_code_symbols",
            &[],
        )
        .context("fetch _hdb_code_symbols")?;
    let file_rows = db
        .query("SELECT node_id, path FROM _hdb_code_files", &[])
        .context("fetch _hdb_code_files")?;
    let repomap_rows = db
        .query(
            "SELECT path, pagerank FROM _hdb_plugin_repomap_cards",
            &[],
        )
        .unwrap_or_default();

    // qualified → (file_id, line_start, line_end). Last-wins on
    // duplicate qualifieds (matches build_symbol_cards's HashMap
    // dedup behaviour).
    let mut sym_meta: HashMap<String, (i64, i64, i64)> = HashMap::with_capacity(sym_rows.len());
    for r in &sym_rows {
        let q = tuple_str(r, 0);
        if q.is_empty() { continue; }
        let fid = tuple_int(r, 1).unwrap_or(0);
        let ls = tuple_int(r, 2).unwrap_or(0);
        let le = tuple_int(r, 3).unwrap_or(0);
        sym_meta.insert(q, (fid, ls, le));
    }
    let file_path: HashMap<i64, String> = file_rows
        .iter()
        .filter_map(|r| {
            let id = tuple_int(r, 0)?;
            let p = tuple_str(r, 1);
            if p.is_empty() { None } else { Some((id, p)) }
        })
        .collect();
    let file_pr: HashMap<String, f64> = repomap_rows
        .iter()
        .filter_map(|r| {
            let p = tuple_str(r, 0);
            if p.is_empty() {
                return None;
            }
            let pr = match r.get(1) {
                Some(heliosdb_nano::Value::Float8(f)) => *f,
                Some(heliosdb_nano::Value::Float4(f)) => *f as f64,
                _ => 0.0,
            };
            Some((p, pr))
        })
        .collect();

    // Build the snapshot list, attaching path + pagerank so we can
    // sort by it. Returned as Tuple-shape items so the rest of the
    // function flows like the old SELECT JOIN result.
    #[derive(Clone)]
    struct CardSnap {
        qualified: String,
        signature: String,
        doc1l: String,
        content_hash: String,
        llm_summary: String,
        llm_model: String,
        path: String,
        line_start: i64,
        line_end: i64,
        pagerank: f64,
    }
    let mut rows: Vec<CardSnap> = Vec::with_capacity(card_rows.len());
    for r in &card_rows {
        let qualified = tuple_str(r, 0);
        if qualified.is_empty() { continue; }
        let (file_id, line_start, line_end) = match sym_meta.get(&qualified) {
            Some(m) => *m,
            None => continue,
        };
        let path = match file_path.get(&file_id) {
            Some(p) => p.clone(),
            None => continue,
        };
        let pagerank = file_pr.get(&path).copied().unwrap_or(0.0);
        rows.push(CardSnap {
            qualified,
            signature: tuple_str(r, 1),
            doc1l: tuple_str(r, 2),
            content_hash: tuple_str(r, 3),
            llm_summary: tuple_str(r, 4),
            llm_model: tuple_str(r, 5),
            path,
            line_start,
            line_end,
            pagerank,
        });
    }
    // PageRank-descending. Stable sort on `qualified` as tiebreaker
    // for deterministic --max-symbols caps across runs.
    rows.sort_by(|a, b| {
        b.pagerank
            .partial_cmp(&a.pagerank)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.qualified.cmp(&b.qualified))
    });
    stats.candidates = rows.len();

    // Pre-load src.content so workers don't all hit the DB
    // concurrently for the same file.
    let mut file_lines: HashMap<String, Vec<String>> = HashMap::new();
    if let Ok(src_rows) = db.query("SELECT path, content FROM src", &[]) {
        for row in &src_rows {
            let path = tuple_str(row, 0);
            let content = tuple_str(row, 1);
            if !path.is_empty() {
                file_lines.insert(path, content.lines().map(str::to_string).collect());
            }
        }
    }

    // Build the work queue. Each item carries everything the worker
    // needs (no DB access from workers — engine isn't yet
    // concurrent-write-friendly anyway).
    #[derive(Clone)]
    #[allow(dead_code)] // content_hash is re-fetched single-threaded by the writer.
    struct Job {
        qualified: String,
        signature: String,
        doc1l: String,
        content_hash: String,
        body_excerpt: String,
    }

    let mut jobs: Vec<Job> = Vec::with_capacity(rows.len());
    for snap in &rows {
        // Idempotency: skip when the existing summary was generated
        // by THIS model from THIS content_hash (encoded into the
        // model-tagged hash we store).
        let model_hash = format!("{}|{}", opts.model, snap.content_hash);
        if !snap.llm_summary.is_empty() && snap.llm_model == model_hash {
            stats.unchanged += 1;
            continue;
        }

        let line_start = snap.line_start as usize;
        let line_end = snap.line_end as usize;
        // Body excerpt: up to 60 lines from the symbol's source range.
        // Keeps Ollama prompt small; Qwen3-coder 30B is fast on short
        // contexts but slows linearly past ~2k tokens.
        let body_excerpt = if line_start > 0 && line_end >= line_start {
            file_lines
                .get(&snap.path)
                .map(|lines| {
                    let from = line_start.saturating_sub(1);
                    let to = (line_end).min(lines.len()).min(from + 60);
                    lines.get(from..to).map(|s| s.join("\n")).unwrap_or_default()
                })
                .unwrap_or_default()
        } else {
            String::new()
        };

        jobs.push(Job {
            qualified: snap.qualified.clone(),
            signature: snap.signature.clone(),
            doc1l: snap.doc1l.clone(),
            content_hash: snap.content_hash.clone(),
            body_excerpt,
        });
        if opts.max_symbols > 0 && jobs.len() >= opts.max_symbols {
            break;
        }
    }

    if jobs.is_empty() {
        return Ok(stats);
    }

    // Worker pool: each thread pulls jobs off a shared queue, posts
    // to Ollama, returns the (qualified, summary, prompt_tokens,
    // completion_tokens) tuple. Errors return None and increment
    // `failed`.
    let queue = Arc::new(Mutex::new(jobs.into_iter()));
    type WorkerResult = Option<(String, String, u64, u64)>;
    let mut handles = Vec::new();
    let opts_arc = Arc::new(opts.clone());
    for _ in 0..opts.concurrency.max(1) {
        let queue = Arc::clone(&queue);
        let opts = Arc::clone(&opts_arc);
        handles.push(thread::spawn(move || -> Vec<WorkerResult> {
            let mut out: Vec<WorkerResult> = Vec::new();
            loop {
                let job = {
                    let mut q = match queue.lock() {
                        Ok(g) => g,
                        Err(_) => return out,
                    };
                    q.next()
                };
                let Some(job) = job else {
                    return out;
                };
                let prompt = build_prompt(&job.qualified, &job.signature, &job.doc1l, &job.body_excerpt);
                match call_ollama(&opts.endpoint, &opts.model, &prompt, opts.max_tokens, opts.timeout_secs) {
                    Ok((summary, pt, ct)) => out.push(Some((job.qualified, summary, pt, ct))),
                    Err(_) => out.push(None),
                }
            }
        }));
    }

    let mut all: Vec<WorkerResult> = Vec::new();
    for h in handles {
        if let Ok(mut v) = h.join() {
            all.append(&mut v);
        }
    }

    // Write back single-threaded (engine is single-writer).
    for r in all {
        match r {
            Some((qualified, summary, pt, ct)) => {
                let summary_trim = summary.trim().to_string();
                if summary_trim.is_empty() {
                    stats.failed += 1;
                    continue;
                }
                // Look up the content_hash so the persisted llm_model
                // tag matches what build_symbol_cards wrote. Use
                // parameterized query — the engine's plain `query`
                // path mangles SQL bytes when the literal contains
                // multibyte UTF-8 (e.g. em-dash in a doc heading).
                let row = db.query_params(
                    &format!("SELECT content_hash FROM {SYMBOL_CARDS_TABLE} WHERE qualified = $1"),
                    &[Value::String(qualified.clone())],
                );
                let content_hash = row
                    .ok()
                    .and_then(|rs| rs.first().map(|r| tuple_str(r, 0)))
                    .unwrap_or_default();
                let model_tag = format!("{}|{}", opts.model, content_hash);
                let update = format!(
                    "UPDATE {SYMBOL_CARDS_TABLE} SET llm_summary = $1, llm_model = $2 WHERE qualified = $3"
                );
                match db.execute_params(
                    &update,
                    &[
                        Value::String(summary_trim.clone()),
                        Value::String(model_tag.clone()),
                        Value::String(qualified.clone()),
                    ],
                ) {
                    Ok(_) => {
                        stats.written += 1;
                        stats.total_prompt_tokens += pt;
                        stats.total_completion_tokens += ct;
                    }
                    Err(_) => stats.failed += 1,
                }
            }
            None => stats.failed += 1,
        }
    }

    Ok(stats)
}

fn build_prompt(qualified: &str, signature: &str, doc1l: &str, body: &str) -> String {
    let mut p = String::new();
    p.push_str(
        "Summarise the following code symbol in EXACTLY ONE sentence. \
         Describe what it does at a high level — no parameter lists, no implementation notes, no preamble. \
         Reply with only the sentence and nothing else.\n\n",
    );
    p.push_str(&format!("Symbol: {qualified}\nSignature: {signature}\n"));
    if !doc1l.is_empty() {
        p.push_str(&format!("Existing one-line doc: {doc1l}\n"));
    }
    if !body.is_empty() {
        // Cap body at a hard ~3000 chars to keep prompts small.
        let cap: String = body.chars().take(3000).collect();
        p.push_str("Body:\n");
        p.push_str(&cap);
    }
    p
}

fn call_ollama(
    endpoint: &str,
    model: &str,
    prompt: &str,
    max_tokens: u32,
    timeout_secs: u64,
) -> Result<(String, u64, u64)> {
    let url = format!("{}/v1/chat/completions", endpoint.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "stream": false,
        "options": { "temperature": 0.0, "num_predict": max_tokens },
    });
    let agent = if timeout_secs > 0 {
        ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .build()
    } else {
        ureq::AgentBuilder::new().build()
    };
    let resp = agent
        .post(&url)
        .set("Content-Type", "application/json")
        .send_json(body)
        .map_err(|e| anyhow::anyhow!("ollama POST failed: {e}"))?;
    let v: serde_json::Value = resp
        .into_json()
        .map_err(|e| anyhow::anyhow!("ollama response parse failed: {e}"))?;
    let summary = v["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let prompt_tokens = v["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
    let completion_tokens = v["usage"]["completion_tokens"].as_u64().unwrap_or(0);
    Ok((summary, prompt_tokens, completion_tokens))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pagerank_converges_on_small_cyclic_graph() {
        // 3-node cycle: 0 → 1 → 2 → 0. Symmetric topology, every
        // node should converge to 1/3.
        let nodes = vec![0i64, 1, 2];
        let mut edges = HashMap::new();
        edges.insert(0, vec![1]);
        edges.insert(1, vec![2]);
        edges.insert(2, vec![0]);
        let (ranks, iters, converged) = pagerank(&nodes, &edges, 50, 0.85, 1e-9);
        assert!(converged, "should converge in {iters} iterations");
        let total: f64 = ranks.values().sum();
        assert!(
            (total - 1.0).abs() < 1e-3,
            "ranks must sum to ~1.0, got {total}"
        );
        for v in [0i64, 1, 2] {
            let r = ranks[&v];
            assert!(
                (r - 1.0 / 3.0).abs() < 1e-3,
                "node {v} rank {r} should be ~0.333"
            );
        }
    }

    #[test]
    fn pagerank_terminates_with_isolated_node() {
        // Node 99 has no in/out edges — should still get the
        // teleport mass.
        let nodes = vec![0i64, 1, 99];
        let mut edges = HashMap::new();
        edges.insert(0, vec![1]);
        edges.insert(1, vec![0]);
        let (ranks, _iters, converged) = pagerank(&nodes, &edges, 50, 0.85, 1e-6);
        assert!(converged);
        // The cycle 0↔1 should have higher rank than the isolated 99.
        assert!(ranks[&0] > ranks[&99]);
        assert!(ranks[&1] > ranks[&99]);
        // Isolated node should still be > 0 (teleport guarantees this).
        assert!(ranks[&99] > 0.0);
    }

    #[test]
    fn pagerank_caps_at_max_iters_when_not_converging() {
        // Hard-to-converge: many sinks. With a strict tolerance and
        // a low cap, we should hit the cap and report converged=false.
        let mut nodes: Vec<i64> = (0..100).collect();
        nodes.extend(200..210); // sinks
        let mut edges = HashMap::new();
        for i in 0..99 {
            edges.insert(i as i64, vec![(i + 1) as i64]);
        }
        let (_ranks, iters, converged) = pagerank(&nodes, &edges, 3, 0.85, 1e-12);
        assert_eq!(iters, 3);
        assert!(!converged, "should report not-converged when cap reached");
    }

    #[test]
    fn extract_doc1l_rust_triple_slash() {
        let lines = vec![
            "// header".to_string(),
            "".to_string(),
            "/// Encodes a frame to wire form.".to_string(),
            "pub fn encode() {}".to_string(),
        ];
        // pub fn at line 4 (1-indexed). Comment immediately above.
        let s = extract_doc1l(&lines, 4);
        assert_eq!(s, "Encodes a frame to wire form.");
    }

    #[test]
    fn extract_doc1l_python_hash_and_blank() {
        let lines = vec![
            "import os".to_string(),
            "".to_string(),
            "# Top-level helper.".to_string(),
            "def go():".to_string(),
        ];
        let s = extract_doc1l(&lines, 4);
        assert_eq!(s, "Top-level helper.");
    }

    #[test]
    fn extract_doc1l_caps_120_chars() {
        let long = "A".repeat(500);
        let lines = vec![format!("// {long}"), "fn f() {}".to_string()];
        let s = extract_doc1l(&lines, 2);
        assert_eq!(s.len(), 120);
        assert!(s.chars().all(|c| c == 'A'));
    }

    #[test]
    fn extract_doc1l_no_comment_returns_empty() {
        let lines = vec!["use std::io;".to_string(), "fn f() {}".to_string()];
        let s = extract_doc1l(&lines, 2);
        assert!(s.is_empty());
    }

    #[test]
    fn extract_doc1l_out_of_bounds_returns_empty() {
        let lines = vec!["fn f() {}".to_string()];
        assert!(extract_doc1l(&lines, 0).is_empty());
        assert!(extract_doc1l(&lines, 999).is_empty());
    }

    #[test]
    fn blake_hex_is_stable_and_collision_avoiding() {
        let a = blake_hex("hello");
        let b = blake_hex("hello");
        assert_eq!(a, b);
        let c = blake_hex("world");
        assert_ne!(a, c);
        assert_eq!(a.len(), 16);
    }

    // Note: build_symbol_cards / build_repomap_cards integration is
    // covered by tests/cards_built_after_ingest.rs which spins a real
    // tempdir KB through `init --ingest` and asserts on row counts +
    // wrapper response shapes.
}
