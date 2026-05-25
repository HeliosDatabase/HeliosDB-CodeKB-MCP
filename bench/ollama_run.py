#!/usr/bin/env python3
"""Self-hosted bench runner — drives qwen3-coder (or another Ollama model)
against the canonical bench questions WITH vs. WITHOUT the heliosdb-codekb
MCP server, using Tailscale-reachable Ollama as the agent backend.

Replaces `claude -p` so the bench can run without paid API spend, on a
deterministic local model. The signal is necessarily different from a
Haiku/Opus run (model capability + tool-call style differ) — read the
comparison as "Qwen3 with vs without MCP," not "Qwen3 vs Haiku."

Per question, the harness runs an agent loop:

    user → assistant (calls tools) → tools' results → assistant → ... → final answer

at most `MAX_TURNS` turns. Token counts come from Ollama's OpenAI-
compatible `/v1/chat/completions` `usage` field on every step.

Environment / CLI:

    OLLAMA_BASE     base URL (default http://ollama:11434)
    OLLAMA_MODEL    model tag (default qwen3-coder:30b)
    BENCH_DIR       output dir (default /tmp/codekb-bench-ollama-YYYYMMDD)
    QUESTIONS       path to questions file (default bench/questions.txt)
    WITH_DIR        source root visible to WITH-MCP runs
    WITHOUT_DIR     source root visible to WITHOUT-MCP runs
    MCP_BIN         path to heliosdb-codekb-mcp binary
    MCP_PROFILE     --profile passed to the MCP server (default standard)
    MCP_STRIP       --strip-tool-descriptions (default 200)
    TRIALS          per-question trials (default 1)
    MAX_TURNS       agent-loop turn cap (default 24)
    MAX_TOOL_BYTES  cap on serialized tool-result string (default 4096)

Outputs `$BENCH_DIR/results/{with,without}/qNN-tT.json` with the same
shape `bench/compare.sh` expects (cost is N/A for self-hosted; we
emit total_cost_usd = 0.0 and surface tokens in `usage`).
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import time
import urllib.request
from datetime import date
from pathlib import Path

# ---------------------------------------------------------------------------
# Config (env-driven; sensible defaults)
# ---------------------------------------------------------------------------


def _find_mcp_binary() -> str | None:
    """Best-effort: look in PATH then $PWD/target/{release,debug}."""
    from shutil import which
    p = which("heliosdb-codekb-mcp")
    if p:
        return p
    for candidate in (
        Path.cwd() / "target/release/heliosdb-codekb-mcp",
        Path.cwd() / "target/debug/heliosdb-codekb-mcp",
    ):
        if candidate.exists():
            return str(candidate)
    return None


OLLAMA_BASE = os.environ.get("OLLAMA_BASE", "http://ollama:11434").rstrip("/")
OLLAMA_MODEL = os.environ.get("OLLAMA_MODEL", "qwen3-coder:30b")
BENCH_DIR = Path(os.environ.get(
    "BENCH_DIR",
    f"/tmp/codekb-bench-ollama-{date.today().isoformat().replace('-', '')}",
))
QUESTIONS = Path(os.environ.get("QUESTIONS", str(Path(__file__).parent / "questions.txt")))
WITH_DIR = os.environ.get("WITH_DIR")
WITHOUT_DIR = os.environ.get("WITHOUT_DIR")
MCP_BIN = os.environ.get("MCP_BIN") or _find_mcp_binary()
MCP_PROFILE = os.environ.get("MCP_PROFILE", "standard")
MCP_STRIP = os.environ.get("MCP_STRIP", "200")
MCP_MEGA = os.environ.get("MCP_MEGA", "0") == "1"
STEER = os.environ.get("STEER", "0") == "1"
STEER_PROMPT_PATH = Path(os.environ.get(
    "STEER_PROMPT",
    str(Path(__file__).parent / "steer-prompt.md"),
))
TRIALS = int(os.environ.get("TRIALS", "1"))
MAX_TURNS = int(os.environ.get("MAX_TURNS", "24"))
MAX_TOOL_BYTES = int(os.environ.get("MAX_TOOL_BYTES", "4096"))


# ---------------------------------------------------------------------------
# Ollama chat (OpenAI-compatible)
# ---------------------------------------------------------------------------


def ollama_chat(messages: list[dict], tools: list[dict] | None) -> dict:
    """POST /v1/chat/completions and return the full JSON response."""
    body = {
        "model": OLLAMA_MODEL,
        "messages": messages,
        "stream": False,
        "options": {"temperature": 0.0, "num_predict": 1024},
    }
    if tools:
        body["tools"] = tools
    data = json.dumps(body).encode()
    req = urllib.request.Request(
        f"{OLLAMA_BASE}/v1/chat/completions",
        data=data,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=300) as resp:
        return json.loads(resp.read().decode())


# ---------------------------------------------------------------------------
# MCP subprocess client (JSON-RPC over stdio)
# ---------------------------------------------------------------------------


class McpStdioClient:
    """Tiny JSON-RPC stdio client for the heliosdb-codekb-mcp `serve` subprocess."""

    def __init__(self, source_dir: str, profile: str = "standard", strip: str = "200"):
        assert MCP_BIN, "MCP_BIN unset — set MCP_BIN=<path-to-heliosdb-codekb-mcp>"
        args = [
            MCP_BIN,
            "serve",
            "--source", source_dir,
            "--profile", profile,
            "--strip-tool-descriptions", strip,
        ]
        if MCP_MEGA:
            args.append("--mega-tool")
        self.proc = subprocess.Popen(
            args,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        self._id = 0

    def _rpc(self, method: str, params: dict | None = None) -> dict:
        self._id += 1
        req = {"jsonrpc": "2.0", "id": self._id, "method": method, "params": params or {}}
        self.proc.stdin.write(json.dumps(req) + "\n")
        self.proc.stdin.flush()
        line = self.proc.stdout.readline()
        if not line:
            raise RuntimeError(f"MCP stdout closed mid-call (method={method})")
        return json.loads(line)

    def list_tools(self) -> list[dict]:
        resp = self._rpc("tools/list", {})
        return resp.get("result", {}).get("tools", [])

    def call_tool(self, name: str, arguments: dict) -> dict:
        resp = self._rpc("tools/call", {"name": name, "arguments": arguments})
        return resp.get("result") or {"isError": True, "error": resp.get("error")}

    def close(self):
        try:
            self.proc.stdin.close()
        except Exception:
            pass
        self.proc.terminate()
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()


def mcp_tools_to_openai(mcp_tools: list[dict]) -> list[dict]:
    """Translate MCP tool descriptors → OpenAI-format tool function defs."""
    out = []
    for t in mcp_tools:
        name = t.get("name")
        if not name:
            continue
        desc = (t.get("description") or "").strip() or name
        schema = t.get("inputSchema") or {"type": "object", "properties": {}}
        out.append({
            "type": "function",
            "function": {
                "name": name,
                "description": desc[:512],   # qwen3-coder gets confused by very long descriptions
                "parameters": schema,
            },
        })
    return out


# ---------------------------------------------------------------------------
# WITHOUT-MCP toolset — local filesystem primitives
# ---------------------------------------------------------------------------


def _safe_join(root: str, rel: str) -> Path:
    """Resolve `rel` under `root`, raising if it escapes the root."""
    base = Path(root).resolve()
    target = (base / rel).resolve() if not Path(rel).is_absolute() else Path(rel).resolve()
    if not str(target).startswith(str(base)):
        raise ValueError(f"path escapes root: {rel}")
    return target


def local_read(args: dict, root: str) -> str:
    path = args.get("path") or args.get("file") or ""
    if not path:
        return "error: missing `path`"
    try:
        target = _safe_join(root, path)
        with open(target, "rb") as f:
            data = f.read(64 * 1024)
        return data.decode("utf-8", errors="replace")
    except Exception as e:
        return f"error: {e}"


def local_glob(args: dict, root: str) -> str:
    pattern = args.get("pattern") or ""
    if not pattern:
        return "error: missing `pattern`"
    base = Path(root)
    matches = [str(p.relative_to(base)) for p in base.rglob(pattern) if p.is_file()][:200]
    return "\n".join(matches) if matches else "(no matches)"


def local_grep(args: dict, root: str) -> str:
    pattern = args.get("pattern") or ""
    path_glob = args.get("path") or "**/*"
    if not pattern:
        return "error: missing `pattern`"
    try:
        rx = re.compile(pattern)
    except re.error as e:
        return f"error: invalid regex: {e}"
    base = Path(root)
    hits = []
    for p in base.rglob(path_glob if "*" in path_glob else f"**/{path_glob}"):
        if not p.is_file():
            continue
        try:
            with open(p, "r", encoding="utf-8", errors="replace") as f:
                for i, line in enumerate(f, 1):
                    if rx.search(line):
                        hits.append(f"{p.relative_to(base)}:{i}:{line.rstrip()}")
                        if len(hits) >= 200:
                            return "\n".join(hits) + "\n(truncated at 200 hits)"
        except Exception:
            continue
    return "\n".join(hits) if hits else "(no matches)"


def local_bash(args: dict, root: str) -> str:
    cmd = args.get("command") or args.get("cmd") or ""
    if not cmd:
        return "error: missing `command`"
    try:
        r = subprocess.run(
            cmd, shell=True, capture_output=True, text=True, cwd=root, timeout=60,
        )
        out = (r.stdout or "") + (("\n[stderr]\n" + r.stderr) if r.stderr else "")
        return out[:8192]
    except subprocess.TimeoutExpired:
        return "error: command timed out"
    except Exception as e:
        return f"error: {e}"


WITHOUT_TOOLS = [
    {
        "type": "function",
        "function": {
            "name": "Read",
            "description": "Read a file from the source root. Args: {path: string}. Returns first 64 KiB.",
            "parameters": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]},
        },
    },
    {
        "type": "function",
        "function": {
            "name": "Glob",
            "description": "Find files matching a glob pattern relative to the source root. Args: {pattern: string}.",
            "parameters": {"type": "object", "properties": {"pattern": {"type": "string"}}, "required": ["pattern"]},
        },
    },
    {
        "type": "function",
        "function": {
            "name": "Grep",
            "description": "Regex search over files. Args: {pattern: string, path?: glob}.",
            "parameters": {"type": "object", "properties": {"pattern": {"type": "string"}, "path": {"type": "string"}}, "required": ["pattern"]},
        },
    },
    {
        "type": "function",
        "function": {
            "name": "Bash",
            "description": "Run a shell command in the source root. 60s timeout, 8 KiB output cap.",
            "parameters": {"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]},
        },
    },
]


def execute_without_tool(name: str, args: dict, root: str) -> str:
    return {
        "Read": local_read,
        "Glob": local_glob,
        "Grep": local_grep,
        "Bash": local_bash,
    }.get(name, lambda *_: f"error: unknown tool {name}")(args, root)


# ---------------------------------------------------------------------------
# Agent loop
# ---------------------------------------------------------------------------

_BASE_SYSTEM_PROMPT = (
    "You are answering a developer's question about the code repository made "
    "available via the tools below. Use the tools to investigate, then return "
    "a concise final answer (3-6 sentences max). Do NOT call tools for things "
    "you already know — only call them to look up specific facts in the repo. "
    "When you need a tool, you MUST use the OpenAI tool_calls JSON format the "
    "API provides — do NOT emit `<function=…>` inline syntax in your message."
)


def _build_system_prompt() -> str:
    """Optionally append the steer-prompt file (parity with the Claude bench's
    --append-system-prompt-file flag). Same prompt is applied to both WITH and
    WITHOUT setups so the comparison stays apples-to-apples."""
    prompt = _BASE_SYSTEM_PROMPT
    if STEER:
        try:
            steer = STEER_PROMPT_PATH.read_text()
            prompt = prompt + "\n\n" + steer.strip()
        except Exception as e:
            print(f"WARN: STEER=1 but could not read {STEER_PROMPT_PATH}: {e}", file=sys.stderr)
    return prompt


SYSTEM_PROMPT = _build_system_prompt()

# Qwen3-coder occasionally emits inline tool-call syntax instead of
# the proper `tool_calls` field on the assistant message. This regex
# catches both common variants so the agent loop can recover.
_INLINE_FUNC_RE = re.compile(
    r"<function=([A-Za-z0-9_]+)>(.*?)</function>",
    re.DOTALL,
)
_INLINE_PARAM_RE = re.compile(
    r"<parameter=([A-Za-z0-9_]+)>(.*?)</parameter>",
    re.DOTALL,
)


def _parse_inline_tool_calls(content: str) -> list[dict] | None:
    """Salvage Qwen3-style inline `<function=…>` calls into OpenAI shape."""
    if not content or "<function=" not in content:
        return None
    out = []
    for i, m in enumerate(_INLINE_FUNC_RE.finditer(content)):
        name = m.group(1)
        body = m.group(2)
        args = {
            p.group(1): p.group(2).strip()
            for p in _INLINE_PARAM_RE.finditer(body)
        }
        out.append({
            "id": f"inline_{i}",
            "type": "function",
            "function": {"name": name, "arguments": json.dumps(args)},
        })
    return out or None


def _truncate(s: str, n: int) -> str:
    if len(s) <= n:
        return s
    return s[:n] + f"\n…[truncated, {len(s)-n} bytes dropped]"


def run_agent(
    question: str,
    tools: list[dict],
    execute_tool,
) -> dict:
    """One full agent loop. Returns {result, turns, prompt_tokens, completion_tokens, total_tool_bytes}."""
    messages = [
        {"role": "system", "content": SYSTEM_PROMPT},
        {"role": "user", "content": question},
    ]
    total_prompt = 0
    total_completion = 0
    total_tool_bytes = 0
    last_text = ""
    for turn in range(1, MAX_TURNS + 1):
        try:
            resp = ollama_chat(messages, tools)
        except Exception as e:
            return {
                "result": f"[ollama error] {e}",
                "turns": turn,
                "usage": {
                    "input_tokens": total_prompt,
                    "output_tokens": total_completion,
                    "cache_read_input_tokens": 0,
                    "cache_creation_input_tokens": 0,
                },
                "tool_bytes": total_tool_bytes,
                "stop_reason": "error",
            }
        usage = resp.get("usage", {})
        total_prompt += int(usage.get("prompt_tokens", 0))
        total_completion += int(usage.get("completion_tokens", 0))
        choice = resp.get("choices", [{}])[0]
        msg = choice.get("message", {})
        finish = choice.get("finish_reason", "")
        content = msg.get("content") or ""
        last_text = content or last_text
        tool_calls = msg.get("tool_calls") or []
        # Fallback: parse Qwen3-style inline <function=…> syntax when
        # the model didn't use the proper tool_calls field.
        if not tool_calls and "<function=" in content:
            recovered = _parse_inline_tool_calls(content)
            if recovered:
                tool_calls = recovered
                # Strip the inline syntax so the next turn doesn't
                # re-emit it.
                content = _INLINE_FUNC_RE.sub("", content).strip()
        # Always append the assistant message first so subsequent
        # tool replies can reference its tool_call_ids.
        messages.append({
            "role": "assistant",
            "content": content,
            "tool_calls": tool_calls if tool_calls else None,
        })
        if not tool_calls:
            return {
                "result": msg.get("content") or last_text,
                "turns": turn,
                "usage": {
                    "input_tokens": total_prompt,
                    "output_tokens": total_completion,
                    "cache_read_input_tokens": 0,
                    "cache_creation_input_tokens": 0,
                },
                "tool_bytes": total_tool_bytes,
                "stop_reason": finish or "stop",
            }
        for call in tool_calls:
            fn = call.get("function", {})
            name = fn.get("name", "")
            try:
                raw_args = fn.get("arguments") or "{}"
                args = json.loads(raw_args) if isinstance(raw_args, str) else raw_args
            except json.JSONDecodeError:
                args = {}
            tool_text = execute_tool(name, args)
            tool_text = _truncate(str(tool_text), MAX_TOOL_BYTES)
            total_tool_bytes += len(tool_text)
            messages.append({
                "role": "tool",
                "tool_call_id": call.get("id", ""),
                "name": name,
                "content": tool_text,
            })
    # Loop cap hit.
    return {
        "result": last_text or "[agent hit max turns without final answer]",
        "turns": MAX_TURNS,
        "usage": {
            "input_tokens": total_prompt,
            "output_tokens": total_completion,
            "cache_read_input_tokens": 0,
            "cache_creation_input_tokens": 0,
        },
        "tool_bytes": total_tool_bytes,
        "stop_reason": "max_turns",
    }


# ---------------------------------------------------------------------------
# Per-question harness
# ---------------------------------------------------------------------------


def load_questions(path: Path) -> list[str]:
    out = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            out.append(line)
    return out


def run_with_mcp(question: str) -> dict:
    if not WITH_DIR:
        raise SystemExit("WITH_DIR env var required for WITH-MCP run")
    client = McpStdioClient(WITH_DIR, MCP_PROFILE, MCP_STRIP)
    try:
        mcp_tools = client.list_tools()
        tools = mcp_tools_to_openai(mcp_tools)

        def exec_tool(name: str, args: dict) -> str:
            res = client.call_tool(name, args)
            if isinstance(res, dict) and res.get("isError"):
                err = res.get("error") or res
                return f"[error] {json.dumps(err)[:512]}"
            # Engine + plugin wrap as {"content":[{"type":"text","text": "..."}]}
            content = res.get("content") if isinstance(res, dict) else None
            if content and isinstance(content, list) and content:
                first = content[0]
                if isinstance(first, dict) and first.get("type") == "text":
                    return first.get("text", "")
            return json.dumps(res)[:8192]

        return run_agent(question, tools, exec_tool)
    finally:
        client.close()


def run_without_mcp(question: str) -> dict:
    if not WITHOUT_DIR:
        raise SystemExit("WITHOUT_DIR env var required for WITHOUT-MCP run")

    def exec_tool(name: str, args: dict) -> str:
        return execute_without_tool(name, args, WITHOUT_DIR)

    return run_agent(question, WITHOUT_TOOLS, exec_tool)


def envelope(question: str, run: dict, setup: str, wall_secs: float) -> dict:
    return {
        "type": "result",
        "is_error": False,
        "session_id": f"bench-ollama-{setup}",
        "total_cost_usd": 0.0,  # self-hosted = $0
        "model": OLLAMA_MODEL,
        "endpoint": OLLAMA_BASE,
        "setup": setup,
        "question": question,
        "result": run.get("result", ""),
        "num_turns": run.get("turns", 0),
        "usage": run.get("usage", {}),
        "tool_bytes": run.get("tool_bytes", 0),
        "stop_reason": run.get("stop_reason", ""),
        "bench": {"wall_secs": int(wall_secs)},
    }


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def main() -> int:
    if not MCP_BIN:
        print("ERROR: heliosdb-codekb-mcp binary not found. Set MCP_BIN=<path>.", file=sys.stderr)
        return 2
    if not WITH_DIR or not WITHOUT_DIR:
        print("ERROR: WITH_DIR and WITHOUT_DIR env vars required.", file=sys.stderr)
        return 2

    BENCH_DIR.mkdir(parents=True, exist_ok=True)
    with_res = BENCH_DIR / "results" / "with"
    without_res = BENCH_DIR / "results" / "without"
    with_res.mkdir(parents=True, exist_ok=True)
    without_res.mkdir(parents=True, exist_ok=True)

    questions = load_questions(QUESTIONS)
    if not questions:
        print(f"ERROR: no questions in {QUESTIONS}", file=sys.stderr)
        return 2

    print(f"=== Ollama bench harness ===")
    print(f"endpoint: {OLLAMA_BASE}")
    print(f"model:    {OLLAMA_MODEL}")
    print(f"trials:   {TRIALS}")
    print(f"questions: {len(questions)}")
    print(f"results:  {BENCH_DIR}")
    print()

    for i, q in enumerate(questions, 1):
        print(f"Q{i:02d}: {q}")
        for t in range(1, TRIALS + 1):
            tag = "" if TRIALS == 1 else f"-t{t}"
            for setup, runner, outdir in (
                ("with", run_with_mcp, with_res),
                ("without", run_without_mcp, without_res),
            ):
                out_file = outdir / f"q{i:02d}{tag}.json"
                print(f"  → {setup} (trial {t})…", flush=True)
                start = time.time()
                try:
                    res = runner(q)
                except Exception as e:
                    res = {
                        "result": f"[harness error] {e}",
                        "turns": 0,
                        "usage": {"input_tokens": 0, "output_tokens": 0},
                        "tool_bytes": 0,
                        "stop_reason": "harness_error",
                    }
                wall = time.time() - start
                env = envelope(q, res, setup, wall)
                with open(out_file, "w") as fp:
                    json.dump(env, fp, indent=2)
                print(
                    f"    {setup}: turns={res.get('turns', 0)}  "
                    f"prompt={res['usage'].get('input_tokens', 0)}  "
                    f"completion={res['usage'].get('output_tokens', 0)}  "
                    f"wall={wall:.1f}s",
                    flush=True,
                )
    print()
    print(f"Done. Results in {BENCH_DIR}/results/")
    print(f"Aggregate with: python3 bench/ollama_compare.py {BENCH_DIR}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
