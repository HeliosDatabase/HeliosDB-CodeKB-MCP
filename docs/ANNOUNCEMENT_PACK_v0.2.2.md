# HeliosDB CodeKB MCP v0.2.2 Announcement Pack

Copy-paste-ready launch copy for `heliosdb-codekb-mcp` v0.2.2.

Links:

- crates.io: https://crates.io/crates/heliosdb-codekb-mcp
- GitHub: https://github.com/dimensigon/heliosdb-codekb-mcp
- Latest release: https://github.com/dimensigon/heliosdb-codekb-mcp/releases/tag/v0.2.2

## Clickbait Titles

1. Your AI Coding Agent Is Reading Too Much Code. We Built an MCP to Fix That.
2. Stop Burning Tokens on `grep`: HeliosDB CodeKB Turns Repos into an Agent-Ready Knowledge Base.
3. Claude Code + Codex Finally Get a Real Code Memory Layer.
4. We Put a Vector+Graph Database Behind MCP. The Token Savings Are Not Subtle.
5. From Repo Sprawl to Agent Context in Minutes: Meet HeliosDB CodeKB MCP.
6. Your Monorepo Is Too Big for Chat. It Is Not Too Big for CodeKB.
7. The MCP Plugin That Gives Claude Code and Codex a Persistent Repo Brain.

## Short Announcement

HeliosDB CodeKB MCP v0.2.2 is live.

It turns a codebase plus docs into an MCP-accessible knowledge base for Claude
Code, Codex, and other agent clients. Instead of dumping files into context, the
agent can ask for compact repo summaries, document outlines, GraphRAG evidence,
symbol cards, AST diffs, and targeted code/navigation answers.

Install:

```bash
cargo install heliosdb-codekb-mcp
```

Try it:

```bash
heliosdb-codekb-mcp init --source . --mode co-located --ingest
heliosdb-codekb-mcp serve --source . --mega-tool
```

Why it matters:

- fewer wasted tokens
- faster repo orientation
- MCP-native for Claude Code and Codex
- powered by HeliosDB-Nano as an embedded code+docs KB
- works with code, Markdown/docs, GraphRAG sections, symbol cards, and agent workflows

Links:

- crates.io: https://crates.io/crates/heliosdb-codekb-mcp
- GitHub: https://github.com/dimensigon/heliosdb-codekb-mcp

## Blog Post

# Your AI Coding Agent Is Reading Too Much Code

AI coding agents are powerful, but most of them still waste context like it is
free.

Ask an agent a question about a large repo and the default pattern is familiar:
search files, open files, skim too much, miss the real dependency, then spend
half the context window carrying around source that was only marginally useful.

`heliosdb-codekb-mcp` is our answer to that problem.

It is an MCP server that turns a repository and its documentation into a compact
knowledge base. Claude Code, Codex, and other MCP clients can query it instead of
blindly reading the filesystem.

## What It Does

HeliosDB CodeKB MCP indexes code and docs into an embedded HeliosDB-Nano
database. Once indexed, an agent can ask for:

- repo summaries
- document outlines
- GraphRAG section retrieval
- symbol cards
- code/navigation answers
- AST diffs
- git summaries
- compact evidence for implementation or review tasks

The important part is not that it can search. The important part is that it can
return the right-shaped context for an agent.

Agents do not always need a full file. Often they need the top sections, a
symbol signature, a short evidence card, or a few references. That is where token
savings show up.

## Why MCP?

MCP gives coding agents a standard way to call external tools. That means the
same repo KB can be exposed to multiple clients, including Claude Code and Codex.

In v0.2.2, CodeKB supports a compact `helios(action, args)` wrapper mode that
keeps tool-list overhead down. Instead of exposing every operation as a large
tool schema, the agent can call one MCP gateway and choose actions like
`repo_summary`, `outline_first`, `doc_drill`, `graphrag_search`, or
`symbol_card`.

## Install

```bash
cargo install heliosdb-codekb-mcp
```

Build a KB:

```bash
heliosdb-codekb-mcp init --source . --mode co-located --ingest
```

Serve it:

```bash
heliosdb-codekb-mcp serve --source . --mega-tool --wrapper-cache-size 128
```

Then connect your MCP client.

## Claude Code Example

```json
{
  "mcpServers": {
    "codekb": {
      "command": "heliosdb-codekb-mcp",
      "args": ["serve", "--source", ".", "--mega-tool", "--wrapper-cache-size", "128"]
    }
  }
}
```

## Codex Example

```toml
[mcp_servers.codekb]
command = "heliosdb-codekb-mcp"
args = ["serve", "--source", ".", "--mega-tool", "--wrapper-cache-size", "128"]
```

## What Makes It Different?

CodeKB is not just a file-search wrapper. It embeds HeliosDB-Nano as a local
database layer, so the KB can combine code graph, document graph, section
retrieval, summaries, and database-backed state.

For small repos, it gives agents faster orientation. For large repos, it becomes
a practical way to keep agent context under control.

## Links

- crates.io: https://crates.io/crates/heliosdb-codekb-mcp
- GitHub: https://github.com/dimensigon/heliosdb-codekb-mcp
- Release: https://github.com/dimensigon/heliosdb-codekb-mcp/releases/tag/v0.2.2

## Reddit: r/rust

Title:

```text
I built an MCP server in Rust that turns a repo into a token-saving code+docs KB for AI coding agents
```

Post:

```text
I just published heliosdb-codekb-mcp v0.2.2.

It is a Rust MCP server that indexes a repo + docs into an embedded HeliosDB-Nano knowledge base, then exposes compact actions to Claude Code, Codex, and other MCP clients.

The goal: stop feeding whole files to agents when they only need a repo summary, doc outline, GraphRAG evidence, symbol card, or AST diff.

Install:

cargo install heliosdb-codekb-mcp

Quick start:

heliosdb-codekb-mcp init --source . --mode co-located --ingest
heliosdb-codekb-mcp serve --source . --mega-tool --wrapper-cache-size 128

Repo:
https://github.com/dimensigon/heliosdb-codekb-mcp

Crate:
https://crates.io/crates/heliosdb-codekb-mcp

I would especially like feedback from Rust devs using Claude Code/Codex on larger repos. What MCP actions would you want exposed next?
```

## Reddit: r/ClaudeAI

Title:

```text
Claude Code keeps reading too many files, so I wired it to a repo knowledge base over MCP
```

Post:

```text
I released heliosdb-codekb-mcp v0.2.2 for Claude Code and other MCP clients.

It turns your repo and docs into a local code+docs knowledge base, then gives Claude compact tools like:

- repo summary
- doc outline
- GraphRAG section search
- symbol card
- AST diff
- git summary

Why I built it: Claude Code is great, but large repos can burn context fast. A KB-backed MCP lets the agent ask for targeted evidence instead of dragging whole files into the conversation.

Install:

cargo install heliosdb-codekb-mcp

Claude Code config:

{
  "mcpServers": {
    "codekb": {
      "command": "heliosdb-codekb-mcp",
      "args": ["serve", "--source", ".", "--mega-tool", "--wrapper-cache-size", "128"]
    }
  }
}

GitHub:
https://github.com/dimensigon/heliosdb-codekb-mcp

Crates:
https://crates.io/crates/heliosdb-codekb-mcp

Would love feedback from people using Claude Code on serious monorepos.
```

## Reddit: r/programming

Title:

```text
Stop dumping your whole repo into AI context: an MCP-backed code knowledge base
```

Post:

```text
I published heliosdb-codekb-mcp v0.2.2.

It is an MCP server for AI coding tools that builds a local knowledge base from a repo and docs, then returns compact context: summaries, doc outlines, GraphRAG evidence, symbol cards, AST diffs, and navigation results.

The idea is simple: the agent should query a structured KB before it starts opening random files.

Install:
cargo install heliosdb-codekb-mcp

Run:
heliosdb-codekb-mcp init --source . --mode co-located --ingest
heliosdb-codekb-mcp serve --source . --mega-tool

Links:
https://github.com/dimensigon/heliosdb-codekb-mcp
https://crates.io/crates/heliosdb-codekb-mcp
```

## X / Twitter

### Tweet 1

```text
🚀 Released heliosdb-codekb-mcp v0.2.2

It turns your repo + docs into an MCP-accessible knowledge base for Claude Code, Codex, and other agents.

Less file dumping.
More targeted evidence.
Fewer wasted tokens.

cargo install heliosdb-codekb-mcp

https://github.com/dimensigon/heliosdb-codekb-mcp

#MCP #RustLang #ClaudeCode #AIcoding #DevTools
```

### Tweet 2

```text
Your AI coding agent is reading too much code 👀

heliosdb-codekb-mcp gives Claude Code/Codex a repo KB over MCP:

✅ repo summaries
✅ doc outlines
✅ GraphRAG evidence
✅ symbol cards
✅ AST diffs
✅ fewer context-window bonfires

https://crates.io/crates/heliosdb-codekb-mcp

#AI #CodingAgents #Rust #MCP
```

### Tweet 3

```text
New MCP plugin: HeliosDB CodeKB 🧠

Index your codebase once.
Let agents query compact context instead of slurping files.

Install:
cargo install heliosdb-codekb-mcp

Serve:
heliosdb-codekb-mcp serve --source . --mega-tool

#ClaudeCode #OpenAI #Codex #RustLang #DeveloperTools
```

## LinkedIn

```text
We released HeliosDB CodeKB MCP v0.2.2.

It gives AI coding agents a persistent, local knowledge layer for codebases and documentation.

Instead of loading large files into context, Claude Code, Codex, and other MCP clients can ask for compact evidence:

- repository summaries
- documentation outlines
- GraphRAG section retrieval
- symbol cards
- AST diffs
- git summaries

This matters because context is now a core developer productivity budget. Large repos need structured retrieval, not blind file walks.

Install:
cargo install heliosdb-codekb-mcp

GitHub:
https://github.com/dimensigon/heliosdb-codekb-mcp

Crates:
https://crates.io/crates/heliosdb-codekb-mcp

#AI #DeveloperTools #Rust #MCP #ClaudeCode #Codex #SoftwareEngineering
```

## Hacker News

Title:

```text
Show HN: HeliosDB CodeKB MCP – a code+docs knowledge base for AI coding agents
```

Post:

```text
Hi HN,

I built heliosdb-codekb-mcp, an MCP server that indexes a repository and its docs into a local knowledge base for AI coding agents.

The goal is to reduce context waste. Instead of opening many files, an agent can ask the MCP for compact context: repo summaries, document outlines, GraphRAG evidence, symbol cards, AST diffs, and git summaries.

It is written in Rust and embeds HeliosDB-Nano as the local KB engine.

Install:
cargo install heliosdb-codekb-mcp

GitHub:
https://github.com/dimensigon/heliosdb-codekb-mcp

Crates:
https://crates.io/crates/heliosdb-codekb-mcp

I am interested in feedback from people using Claude Code, Codex, Cursor, Continue, or other MCP-enabled coding workflows.
```

## Discord / Slack

```text
🚀 HeliosDB CodeKB MCP v0.2.2 is live

It gives Claude Code, Codex, and MCP-compatible agents a local code+docs knowledge base:

• repo summaries
• doc outlines
• GraphRAG evidence
• symbol cards
• AST diffs
• token-saving compact tool mode

Install:
`cargo install heliosdb-codekb-mcp`

Run:
`heliosdb-codekb-mcp init --source . --mode co-located --ingest`
`heliosdb-codekb-mcp serve --source . --mega-tool --wrapper-cache-size 128`

GitHub: https://github.com/dimensigon/heliosdb-codekb-mcp
Crates: https://crates.io/crates/heliosdb-codekb-mcp
```

## Product Hunt

Tagline:

```text
A token-saving codebase knowledge layer for Claude Code, Codex, and MCP agents.
```

Launch copy:

```text
HeliosDB CodeKB MCP turns your repo and docs into an MCP-accessible knowledge base.

Instead of forcing AI coding agents to read whole files, it serves compact repo summaries, document outlines, GraphRAG evidence, symbol cards, AST diffs, and git summaries.

Built in Rust. Powered by embedded HeliosDB-Nano.

Best for teams using Claude Code, Codex, Cursor, Continue, or any MCP-compatible agent on large or fast-moving repositories.
```

## GitHub Release Notes Addendum

## v0.2.2 launch notes

HeliosDB CodeKB MCP v0.2.2 focuses on agent installation and token-saving usage:

- ready-to-use Claude Code and Codex config templates
- compact `helios(action, args)` MCP wrapper mode
- wrapper cache support for repeated calls
- docs/source retrieval workflows for repo and portfolio KBs
- reproducible examples for code+docs KB setup

Install:

```bash
cargo install heliosdb-codekb-mcp
```

Serve:

```bash
heliosdb-codekb-mcp serve --source . --mega-tool --wrapper-cache-size 128
```
