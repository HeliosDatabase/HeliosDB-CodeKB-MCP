---
title: Engine FRs from heliosdb-codekb-mcp Tier C round-up
from: gpc001ca / codekb:0.0 (Opus 4.7)
to: helios:Nano (engine team)
date: 2026-05-26
related:
  - bench/README.md "What's missing to reach 80-90%" (Tier C section)
  - prior FR: ENGINE_REGRESSION_v3.22.2_to_v3.30.0.md (T1 — closed in v3.31.2)
status: REQUEST FOR PLANNING — please ack which ones are tractable, in what order,
        and what we should retest from this side after each lands.
---

# Four engine FRs blocking codekb-mcp's path to 80–90% token savings

The plugin shipped four token-saving layers + a `--mega-tool` gateway
mode (`f6a8aa9` on github.com/HeliosDatabase/HeliosDB-CodeKB-MCP). The
mega-tool is the biggest single lever — collapses tools/list payload
from ~6 KB to ~745 B and cuts WITH-MCP tokens by ~42% on the smoke
corpus.

But the next round of wins is blocked on engine behaviour we
worked around but can't fix from outside. **Four asks, ranked by
plugin-side impact.** Each has a concrete repro plus a sketch of the
fix shape we'd want.

---

## FR #1 (highest impact) — FK validation throughput on user-table writes

### Symptom

Indexing `/home/gpc/HDB` (~260 MB / 10 k files, polyglot Rust+md
corpus) blocked the bench. Three runs we attempted:

| corpus                       | files | code-graph wall | result                              |
|------------------------------|------:|----------------:|-------------------------------------|
| /home/gpc/HDB (full)         | 10344 | > 20 min        | killed; never completed             |
| Nano + Lite + plugin (mixed) |  1962 |  14 min         | distill never completed (>26 min)   |
| smoke (codekb-mcp self)      |    43 |  ~ 1.5 s        | full pipeline in ~12 s              |

The walk + symbol-extract phase is fine; the slowness is concentrated
in (a) `_hdb_code_symbol_refs` writes during `code_index`, and (b)
single-row `INSERT … ON CONFLICT … DO UPDATE` on user tables (see FR
#2 — the plugin's `_hdb_plugin_symbol_cards` UPSERT pace caps at
roughly 200 rows/sec on the bench host, ~5 ms per row, in
`execute_params`).

The plugin already uses `bulk_load_mode = true` + `execute_batch`
batching of 50 statements (see `src/distill.rs::flush_batch` modelled
on `src/linker.rs::link_mentions_bulk`). Throughput still tops out
at the per-row engine cost — somewhere inside the write path the
per-row FK check is the gating call.

### Suggested fix shape

T1 (closed in v3.31.2) addressed the in-txn `_hdb_code_files`
validation path. We're now hitting a different per-row cost — likely
the FK validation on `_hdb_code_symbol_refs.from_symbol`,
`.to_symbol`, `.file_id` and the equivalent on plugin-owned tables
referencing `_hdb_code_files.node_id` / `_hdb_code_symbols.node_id`.

Three angles to consider (engine team picks one):

1. **Batch-validate FK targets per `execute_batch` call** — collect
   the parent-row IDs across all rows in the batch, validate the
   set in one bloom-filter / index probe, then proceed. Skips
   per-row work entirely when all parents are present.
2. **Skip FK validation under `SET bulk_load_mode = true`**, with
   the deferred-validation at COMMIT semantics — this is what most
   bulk-load systems do. The plugin already opts into this mode.
3. **Lazy FK index** — build the parent-row Bloom filter on the
   first FK probe in a session, cache it. Subsequent probes hit
   memory.

### What we'd retest after a fix

```bash
# Full corpus ingest under 5 minutes
BENCH=/tmp/codekb-bench-hdb-fk-fix
rsync -a --exclude=target --exclude=.git --exclude=.fastembed_cache \
  /home/gpc/HDB/ "$BENCH/full-with/"
heliosdb-codekb-mcp init --source "$BENCH/full-with" --mode global --ingest
# Target: code-graph phase <= 2 min, full ingest <= 5 min
```

---

## FR #2 — multi-row `VALUES (a),(b),(c)` INSERT on user tables

### Symptom

`crate::linker` uses multi-row `INSERT INTO _hdb_graph_edges
(from_node, to_node, edge_kind, weight) VALUES (1,2,'M',1), (3,4,'M',1), ...`
and that works at scale (~12 M edges in 84 s).

The same syntax on user-defined tables errors out:

```
Operator not yet implemented: Insert { table_name: "_hdb_plugin_symbol_cards",
  columns: Some(["qualified","signature","doc1l","content_hash"]),
  values: [[Literal(String(...))], [Literal(String(...))]],
  on_conflict: None }
```

### Repro

```bash
helios --source <kb> --profile full -- \
  -c "CREATE TABLE t (a TEXT, b TEXT);
      INSERT INTO t (a, b) VALUES ('1','x'),('2','y');"
# expected: Inserted 2
# actual:   Operator not yet implemented: Insert { ... values: [[…],[…]] }
```

### Plugin workaround

`src/distill.rs::bulk_upsert` emits **one** INSERT per row, batched
50 stmts per `execute_batch`. Functional but ~10× more SQL text than
the equivalent multi-row form would carry.

### Suggested fix shape

Extend the planner / executor case in `Insert { ... values: Vec<Vec<…>> }`
to iterate when `values.len() > 1`, reusing the existing single-row
path per inner Vec. (Already implemented for `_hdb_graph_edges`
in `crate::graph_rag`'s bulk writer? — if yes, hoist that to the
generic Insert path.)

### Retest

After the fix: change `src/distill.rs::bulk_upsert`'s "Phase B" loop
to emit one `INSERT … VALUES (…),(…),(…)` per `ROWS_PER_INSERT_STMT`
(500 rows/stmt). Expect distill phase wall-time to drop ~10× on
mid-size corpora (the engine's plan + parse overhead is amortised
across the value list).

---

## FR #3 — SQL parser miscounts column position on multibyte string literals

### Symptom

Two distinct manifestations, same underlying cause (parser tracks
position in **bytes** but expects **chars** when computing the
"closing quote" location):

1. **Panic** via `db.query_params(... &[Value::String(em_dash_str)])`:

   ```
   thread 'main' panicked at /home/gpc/.cargo/.../graph_rag/with_context.rs:125:43:
   end byte index 75 is not a char boundary; it is inside '—' (bytes 74..77)
   of `select content_hash from _hdb_plugin_symbol_cards where qualified =
        'gate — do not run until engine t1 lands'`
   ```

2. **Parse error** via `db.execute_batch(["DELETE FROM ... WHERE x IN ('a—b','c—d')"])`:

   ```
   SQL parse error: Failed to parse SQL: sql parser error:
   Unterminated string literal at Line: 1, Column: 111
   ```

   Column 111 is reported on a well-formed single-line statement
   whose only "anomaly" is em-dashes (U+2014, 3 bytes UTF-8) inside
   the string literals. With the em-dashes replaced by `-`, the
   exact same statement parses cleanly.

### Plugin workaround

`src/distill.rs::sanitize` maps every non-ASCII char to an ASCII
fallback (`—` → `-`, `'` → `'`, `…` → `.`, `→` → `-`, etc.) before
emitting SQL. Works but loses typography from doc-heading titles.

### Suggested fix shape

`with_context.rs:125:43` — change the byte-index arithmetic to use
`String::is_char_boundary` / `String::char_indices()` instead of
raw byte offsets.

For the sqlparser path: confirm the parser tracks column in `chars()`
(or `grapheme_clusters()`), not `len()`. If sqlparser-rs upstream
has the bug, that's a different patch surface.

### Retest

After the fix:

```rust
// Remove the sanitize() call in src/distill.rs::cap
fn cap(s: String) -> String {
    if s.len() <= MAX_FIELD_BYTES { return s; }
    let mut cut = MAX_FIELD_BYTES;
    while cut > 0 && !s.is_char_boundary(cut) { cut -= 1; }
    s[..cut].to_string()
}

// Then re-run smoke ingest; expect symbol cards to preserve em-dashes,
// smart quotes, etc. in `top_symbols` JSON without breaking parse.
```

---

## FR #4 — `tools/list verbose=false` mode that returns name+desc only

### Symptom

Per-tool schemas dominate the catalogue size. Measured on the codekb
smoke KB:

| profile / mode                        | tools/list bytes |
|---------------------------------------|-----------------:|
| `--profile full --strip-tool-descriptions none` (engine native)  | 10 795 |
| `--profile standard --strip-tool-descriptions 200` (plugin trim) |  5 689 |
| `--profile minimal --strip-tool-descriptions 200`                | ~3 500 |
| `--mega-tool` (plugin replaces with 1 entry)                     |    745 |

The `--mega-tool` path is the smallest by far but loses agent-side
tool-call validation (single dispatcher → less precise tool
selection by the model). A native `tools/list?verbose=false` that
returned just `{name, description}` per tool would let agents see
the full surface while paying ~5–10× less per-turn cache.

### Suggested fix shape

Add `verbose: false` to the existing `tools/list` params:

```rust
// heliosdb_nano::mcp::rpc::handle_rpc_opt — tools/list branch:
let verbose = req.params.get("verbose")
    .and_then(|v| v.as_bool())
    .unwrap_or(false); // <-- already there, default true today
let tools = if verbose {
    tools::list_tools_full()        // current behaviour
} else {
    tools::list_tools_minimal()     // new: name + short description, no inputSchema
};
```

The minimal serialiser drops `inputSchema` (or returns
`{"type":"object"}` as a stub) — the agent calls `tools/list?verbose=true`
on demand for the schema of a specific tool.

### Retest

```bash
# Before:
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"verbose":false}}' \
  | heliosdb-codekb-mcp serve --source <kb> --profile full | wc -c
# Currently returns the same payload as verbose:true. After fix:
# expect ~1.5–2 KB (name + short description × N tools).
```

---

## Priority + scope ask

If the engine team can pick one to land first, **FR #1** unlocks
real-corpus benchmarks (we literally cannot measure the plugin's
mega-tool win at scale without it). FR #2 is the second-biggest
plugin-side win once FR #1 is done. FR #3 is a polish item (we
have a workaround). FR #4 is parallel to the plugin's `--mega-tool`
— either layer works alone, both together is best.

After each FR ships:
1. We bump the plugin's `heliosdb-nano` version in
   `Cargo.toml`.
2. Re-run `bench/run.sh` matrix (smoke + the real corpus).
3. Publish the delta to `bench/README.md` and notify back.

Plugin side will pause Tier B (helios_outline_first auto-drill,
install UX, helios_status, PQ-HNSW adoption) until at least FR #1
lands — Tier B without honest scale numbers from FR #1 would ship
features we can't measure.

— Claude on codekb:0.0 (Opus 4.7)
