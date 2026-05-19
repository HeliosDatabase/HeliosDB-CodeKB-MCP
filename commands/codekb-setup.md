---
description: First-run setup for the heliosdb-codekb-mcp plugin — installs the binary if needed, registers the KB for the current project, and indexes the source tree.
---

You are guiding the user through first-time setup of the **heliosdb-codekb** plugin for the project at `${CLAUDE_PROJECT_DIR}`. Follow these steps in order; do **not** skip steps, but pause for the user's answer at the marked prompt.

## 1. Verify the binary

Check if `heliosdb-codekb-mcp` is on the user's PATH:

```bash
command -v heliosdb-codekb-mcp || echo "NOT_INSTALLED"
```

If the binary is missing **and** the user is on Linux x86_64 (`uname -ms` shows `Linux x86_64`), offer to download v0.1.0 to `~/.local/bin` and `chmod +x` it:

```bash
mkdir -p ~/.local/bin
curl -L https://github.com/dimensigon/heliosdb-codekb-mcp/releases/download/v0.1.0/heliosdb-codekb-mcp-linux-x86_64 \
  -o ~/.local/bin/heliosdb-codekb-mcp
chmod +x ~/.local/bin/heliosdb-codekb-mcp
```

For macOS / aarch64 / other platforms: tell the user pre-built binaries are not yet published and offer to `cargo install --git https://github.com/dimensigon/heliosdb-codekb-mcp --features native-binary-docs` instead. Confirm before running.

## 2. Ask about embeddings

**Prompt the user verbatim:**

> Enable in-process embeddings? One-time **~30 MB download** of the FastEmbedder model (BGE-Small-EN-V1.5, 384-dim) to `$XDG_CACHE_HOME/.fastembed_cache`.
>
> **Benefits:** lifts retrieval quality on paraphrase-style queries — "how does auth work" matches code/docs even when "auth" isn't the literal word in the symbol.
> **Skip if:** you mostly do exact-name lookups (`grep`-style) or want the leanest install.
>
> Default: **yes** (you can disable later by re-running `/codekb-setup` and choosing no).

Wait for the user's answer. Save it as `WITH_EMBEDDINGS=yes` or `WITH_EMBEDDINGS=no`.

## 3. Pick the KB location mode

For most users, **co-located** is the right answer: the KB lives at `${CLAUDE_PROJECT_DIR}/.helios-kb` and is auto-added to `.gitignore`. If the project is on a slow / network-mounted filesystem, suggest `global` instead (KB at `~/.local/share/helios-kb/<slug>`).

Default to `co-located` unless the user asks otherwise.

## 4. Register the KB

```bash
heliosdb-codekb-mcp init --source "${CLAUDE_PROJECT_DIR}" --mode co-located
```

(Substitute the chosen mode if the user picked something else.)

## 5. Run the ingest

If `WITH_EMBEDDINGS=yes`, use **background-quality** mode so the agent gets the fast pass in ~26 s on a typical repo and the embedding pass runs detached:

```bash
heliosdb-codekb-mcp ingest --source "${CLAUDE_PROJECT_DIR}" --background-quality
```

If `WITH_EMBEDDINGS=no`, just the fast pass:

```bash
heliosdb-codekb-mcp ingest --source "${CLAUDE_PROJECT_DIR}"
```

## 6. Confirm and hand off

After ingest exits, run:

```bash
heliosdb-codekb-mcp status --source "${CLAUDE_PROJECT_DIR}"
```

Tell the user:
- The KB is ready and the `helios_*` MCP tools should appear in their next message's tool list.
- If they ran `--background-quality`, paraphrase queries will improve once the child finishes (typically a few minutes); `/codekb-status` shows progress.
- Suggest a starter query like `helios_graphrag_search` with the user's most-asked concept ("authentication", "ingest", whatever fits their project) to confirm it's working.

## Honest caveat

> Engine FK regression v3.28.0+: on large repos (>~500 source files), the indexer's write phase is currently very slow (~93 min on a 700-file repo) because per-write FK validation falls back to a linear scan inside the ingest transaction. Fix is in flight engine-side (T1 in-txn ART overlay). For smaller projects (<200 files) ingest finishes in seconds. See `ENGINE_REGRESSION_v3.22.2_to_v3.30.0.md` for the tracking issue.
