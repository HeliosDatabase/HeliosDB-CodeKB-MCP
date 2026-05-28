//! Plugin-side bulk replacement for `heliosdb_nano::graph_rag::
//! link_exact_qualified`.
//!
//! Why: on the pilot Nano corpus (~700 code files, 18 952 symbols,
//! 2 576 text nodes), the engine's per-row INSERT path took ~89 min
//! to emit 70 582 `MENTIONS` edges — each `INSERT` is its own implicit
//! transaction with FK validation, SMFI delta updates, MV delta
//! tracking, and speculative filter updates. This module does the same
//! computation but:
//!
//! 1. Builds the `(from, to)` pair set entirely in memory.
//! 2. Streams batches of `INSERT … VALUES (…), (…), …` (500 rows
//!    per statement by default) to a tempfile, so very large corpora
//!    don't blow up the heap with SQL text.
//! 3. Wraps the apply pass in `SET bulk_load_mode = true` …
//!    `execute_batch` … `SET bulk_load_mode = false`, which the
//!    engine documents as skipping per-row SMFI / MV / speculative-
//!    filter delta tracking (`heliosdb_nano::storage::engine::
//!    set_bulk_load_mode`).
//!
//! The matching algorithm itself mirrors the engine's
//! `link_exact_qualified`: whole-word match of `_hdb_code_symbols.
//! qualified` inside `_hdb_graph_nodes.{title,text}` for the same
//! text-bearing kinds (`DocChunk`, `DocSection`, `Email`, `Issue`,
//! `Comment`, `InvestorQuestion`, `Answer`). Idempotent: re-running
//! against the same corpus produces no new edges because the
//! pre-existing `MENTIONS` set is loaded first and used to gate the
//! emit step.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, BufWriter, Write};

use anyhow::{Context, Result};
use heliosdb_nano::graph_rag::LinkerStats;
use heliosdb_nano::{EmbeddedDatabase, Value};

/// Text-bearing node kinds the linker scans. Mirrors the engine's
/// `link_exact_qualified` default kind list.
const TEXT_KINDS: &[&str] = &[
    "DocChunk",
    "DocSection",
    "Email",
    "Issue",
    "Comment",
    "InvestorQuestion",
    "Answer",
];

/// Number of `(from, to)` tuples per `INSERT … VALUES (…), (…)`
/// statement. Picked empirically: large enough to amortise the
/// per-statement planner+executor overhead, small enough that one
/// failed row inside a batch doesn't lose a meaningful chunk of work
/// on rollback.
const VALUES_PER_STATEMENT: usize = 500;

/// Number of multi-row statements per `execute_batch` call. The batch
/// runs in one transaction, so this is the granularity at which the
/// engine commits to disk. Keep this × VALUES_PER_STATEMENT × ~80
/// bytes-per-row well under any reasonable WAL flush window.
const STATEMENTS_PER_BATCH: usize = 50;

pub fn link_mentions_bulk(db: &EmbeddedDatabase) -> Result<LinkerStats> {
    let mut stats = LinkerStats::default();

    // 1. qualified → Vec<symbol_id>. Case-sensitive: `Foo`/`foo`
    //    shouldn't collide.
    let mut by_name: HashMap<String, Vec<i64>> = HashMap::new();
    for row in db
        .query(
            "SELECT qualified, node_id FROM _hdb_code_symbols \
             WHERE qualified IS NOT NULL AND qualified <> ''",
            &[],
        )
        .context("load _hdb_code_symbols for linker")?
    {
        let Some(Value::String(name)) = row.values.first().cloned() else {
            continue;
        };
        let Some(sid) = to_int(row.values.get(1)) else {
            continue;
        };
        by_name.entry(name).or_default().push(sid);
    }
    if by_name.is_empty() {
        return Ok(stats);
    }

    // 2. code_symbol_id → graph_node_id, via the `code_symbol:<id>`
    //    source_ref convention used by `graph_rag::project_code_symbols`.
    let mut code_to_graph: HashMap<i64, i64> = HashMap::new();
    for row in db
        .query("SELECT source_ref, node_id FROM _hdb_graph_nodes", &[])
        .context("load _hdb_graph_nodes for linker")?
    {
        let Some(Value::String(sref)) = row.values.first() else {
            continue;
        };
        let Some(gid) = to_int(row.values.get(1)) else {
            continue;
        };
        if let Some(id_str) = sref.strip_prefix("code_symbol:") {
            if let Ok(code_id) = id_str.parse::<i64>() {
                code_to_graph.insert(code_id, gid);
            }
        }
    }

    // 3. Existing MENTIONS — never re-emit a duplicate (idempotency).
    let mut seen: HashSet<(i64, i64)> = HashSet::new();
    for row in db
        .query(
            "SELECT from_node, to_node FROM _hdb_graph_edges WHERE edge_kind = 'MENTIONS'",
            &[],
        )
        .context("load existing MENTIONS edges for dedupe")?
    {
        if let (Some(f), Some(t)) = (to_int(row.values.first()), to_int(row.values.get(1))) {
            seen.insert((f, t));
        }
    }

    // 4. Text-bearing nodes. Whole-word match each qualified name
    //    against `title || '\n' || text`, then collect (from, to)
    //    pairs into a tempfile as we go so very large corpora can't
    //    blow up the heap with pending SQL text.
    let kind_list = TEXT_KINDS
        .iter()
        .map(|k| format!("'{k}'"))
        .collect::<Vec<_>>()
        .join(",");

    let tmp = tempfile::Builder::new()
        .prefix("helios-linker-")
        .suffix(".sql")
        .tempfile()
        .context("create tempfile for linker batch SQL")?;
    let mut writer = BufWriter::new(
        tmp.as_file()
            .try_clone()
            .context("clone tempfile handle for writer")?,
    );

    let mut row_buf: Vec<(i64, i64)> = Vec::with_capacity(VALUES_PER_STATEMENT);
    let flush_statement =
        |buf: &mut Vec<(i64, i64)>, w: &mut BufWriter<std::fs::File>| -> Result<()> {
            if buf.is_empty() {
                return Ok(());
            }
            // INSERT INTO _hdb_graph_edges (from_node, to_node, edge_kind, weight) VALUES (a,b,'MENTIONS',1.0), ... ;\n
            write!(
                w,
                "INSERT INTO _hdb_graph_edges (from_node, to_node, edge_kind, weight) VALUES "
            )?;
            for (i, (from, to)) in buf.iter().enumerate() {
                if i > 0 {
                    w.write_all(b", ")?;
                }
                write!(w, "({from}, {to}, 'MENTIONS', 1.0)")?;
            }
            w.write_all(b";\n")?;
            buf.clear();
            Ok(())
        };

    for row in db
        .query(
            &format!(
                "SELECT node_id, title, text FROM _hdb_graph_nodes \
                 WHERE node_kind IN ({kind_list})"
            ),
            &[],
        )
        .context("load text-bearing nodes for linker")?
    {
        stats.nodes_scanned += 1;
        let Some(node_id) = to_int(row.values.first()) else {
            continue;
        };
        let title = as_string(row.values.get(1)).unwrap_or_default();
        let text = as_string(row.values.get(2)).unwrap_or_default();
        if title.is_empty() && text.is_empty() {
            continue;
        }
        let haystack = format!("{title}\n{text}");

        for (needle, sym_ids) in &by_name {
            if needle.is_empty() {
                continue;
            }
            if !contains_whole_word(&haystack, needle) {
                continue;
            }
            stats.candidates_seen += 1;
            for sid in sym_ids {
                let Some(gid) = code_to_graph.get(sid) else {
                    continue;
                };
                if !seen.insert((node_id, *gid)) {
                    continue; // already present (either pre-existing or
                              // emitted earlier in this pass)
                }
                row_buf.push((node_id, *gid));
                stats.mentions_added += 1;
                if row_buf.len() >= VALUES_PER_STATEMENT {
                    flush_statement(&mut row_buf, &mut writer)?;
                }
            }
        }
    }
    flush_statement(&mut row_buf, &mut writer)?;
    writer.flush().context("flush linker tempfile")?;
    drop(writer);

    if stats.mentions_added == 0 {
        return Ok(stats);
    }

    // 5. Apply pass: SET bulk_load_mode, read+execute in batches,
    //    SET bulk_load_mode = false. Best-effort on the SET calls —
    //    older engine versions don't recognise the setting; ingest
    //    proceeds either way.
    let bulk_enabled = db.execute("SET bulk_load_mode = true").is_ok();
    let apply_result = apply_from_tempfile(db, tmp.path());
    if bulk_enabled {
        let _ = db.execute("SET bulk_load_mode = false");
    }
    apply_result?;

    Ok(stats)
}

fn apply_from_tempfile(db: &EmbeddedDatabase, path: &std::path::Path) -> Result<()> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("re-open linker tempfile {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut batch: Vec<String> = Vec::with_capacity(STATEMENTS_PER_BATCH);
    for line in reader.lines() {
        let stmt = line.context("read linker tempfile line")?;
        if stmt.is_empty() {
            continue;
        }
        batch.push(stmt);
        if batch.len() >= STATEMENTS_PER_BATCH {
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
    db.execute_batch(&refs).with_context(|| {
        format!(
            "execute_batch({} statements) for MENTIONS bulk load",
            refs.len()
        )
    })?;
    Ok(())
}

fn to_int(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Int4(n)) => Some(*n as i64),
        Some(Value::Int8(n)) => Some(*n),
        _ => None,
    }
}

fn as_string(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

/// Whole-word match: `needle` must not be flanked by identifier chars
/// on either side. Mirrors `heliosdb_nano::graph_rag::linker::
/// contains_whole_word`, with one bug fix: advance `start` past the
/// matched needle rather than by one byte, since `+1` can land
/// inside a multi-byte UTF-8 sequence and panic the next slice
/// (real-world repro: doc text containing an emoji adjacent to text
/// scanned for symbol matches).
fn contains_whole_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let mut start = 0usize;
    while let Some(pos) = haystack[start..].find(needle) {
        let abs = start + pos;
        let before_ok =
            abs == 0 || !is_ident_char(haystack.as_bytes().get(abs - 1).copied().unwrap_or(b' '));
        let after_idx = abs + needle.len();
        let after_ok = after_idx == haystack.len()
            || !is_ident_char(haystack.as_bytes().get(after_idx).copied().unwrap_or(b' '));
        if before_ok && after_ok {
            return true;
        }
        // Skip past the matched needle. `abs + needle.len()` is
        // guaranteed to be a char boundary because `abs..abs+len`
        // covered a complete UTF-8 substring (the matched needle).
        // `abs + 1` would NOT be safe — it could land inside a
        // multi-byte char and panic the next `haystack[start..]`.
        start = abs + needle.len();
        if start >= haystack.len() {
            break;
        }
    }
    false
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b':' || b == b'.'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_word_basic() {
        assert!(contains_whole_word("call add(1, 2)", "add"));
        assert!(!contains_whole_word("address book", "add"));
        assert!(!contains_whole_word("padded", "add"));
        assert!(contains_whole_word("see Foo::bar", "Foo::bar"));
    }

    #[test]
    fn whole_word_survives_multibyte_chars_after_failed_match() {
        // Repro of the crash that took down the bench setup on the
        // Full corpus: a partial-but-rejected match (the needle is
        // inside another identifier) immediately followed by an
        // emoji would cause `start = abs + 1` to land mid-emoji and
        // panic the next slice. With the fix (advance by
        // needle.len()), the search just keeps going.
        let h = "padded💰 then add() really";
        assert!(contains_whole_word(h, "add"));
    }

    #[test]
    fn whole_word_no_panic_when_only_match_is_inside_emoji_neighbour() {
        // No real match — function must return false without
        // panicking even when the haystack contains many emojis
        // around the would-be match positions.
        let h = "💰💰💰address💰💰💰";
        assert!(!contains_whole_word(h, "add"));
    }
}
