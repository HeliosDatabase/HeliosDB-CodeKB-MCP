#!/usr/bin/env python3
"""Aggregate Ollama bench output written by `ollama_run.py`.

Reads `<bench_dir>/results/{with,without}/qNN[-tT].json`, prints a
markdown table per question + a totals row. Compared metric is total
*model* tokens (prompt + completion) — there's no cache-read concept
on Ollama, so the comparison is straightforward.

Usage:
    python3 bench/ollama_compare.py /tmp/codekb-bench-ollama-YYYYMMDD [phase-label]
"""

from __future__ import annotations

import json
import re
import statistics
import sys
from collections import defaultdict
from pathlib import Path


def load_one(path: Path) -> dict | None:
    try:
        with open(path) as f:
            return json.load(f)
    except Exception:
        return None


def question_key(filename: str) -> tuple[str, str]:
    # Format: qNN.json or qNN-tT.json
    m = re.match(r"(q\d{2})(?:-(t\d+))?\.json$", filename)
    if not m:
        return ("", "")
    return (m.group(1), m.group(2) or "t1")


def aggregate(results_dir: Path) -> dict[str, dict[str, list]]:
    """Returns {qNN: {setup: [run-dicts]}}."""
    out: dict[str, dict[str, list]] = defaultdict(lambda: defaultdict(list))
    for setup_dir in ("with", "without"):
        d = results_dir / setup_dir
        if not d.is_dir():
            continue
        for f in sorted(d.glob("q*.json")):
            qid, _trial = question_key(f.name)
            if not qid:
                continue
            v = load_one(f)
            if v:
                out[qid][setup_dir].append(v)
    return out


def median_int(values: list[int | float]) -> int:
    return int(statistics.median(values)) if values else 0


def fmt_int(n: int | float) -> str:
    return f"{int(n):,}"


def emit_table(by_q: dict[str, dict[str, list]], label: str) -> str:
    lines = []
    lines.append(f"# Ollama bench summary — {label}")
    lines.append("")
    lines.append(
        "| Q | with prompt | with completion | with turns | with wall | "
        "no-MCP prompt | no-MCP completion | no-MCP turns | no-MCP wall | "
        "Δ total tokens (with − no) |"
    )
    lines.append(
        "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|"
    )

    totals = defaultdict(int)
    for qid in sorted(by_q.keys()):
        with_runs = by_q[qid].get("with", [])
        without_runs = by_q[qid].get("without", [])

        def med(runs, key):
            return median_int([_get(r, key) for r in runs])

        w_p = med(with_runs, "input")
        w_c = med(with_runs, "output")
        w_t = med(with_runs, "turns")
        w_w = med(with_runs, "wall")
        wo_p = med(without_runs, "input")
        wo_c = med(without_runs, "output")
        wo_t = med(without_runs, "turns")
        wo_w = med(without_runs, "wall")
        delta = (w_p + w_c) - (wo_p + wo_c)

        totals["with_p"] += w_p
        totals["with_c"] += w_c
        totals["wo_p"] += wo_p
        totals["wo_c"] += wo_c
        totals["with_w"] += w_w
        totals["wo_w"] += wo_w

        lines.append(
            f"| {qid} | {fmt_int(w_p)} | {fmt_int(w_c)} | {w_t} | {w_w}s | "
            f"{fmt_int(wo_p)} | {fmt_int(wo_c)} | {wo_t} | {wo_w}s | "
            f"{'+' if delta >= 0 else ''}{fmt_int(delta)} |"
        )

    # Totals row (sum of per-Q medians).
    w_tot = totals["with_p"] + totals["with_c"]
    wo_tot = totals["wo_p"] + totals["wo_c"]
    delta_tot = w_tot - wo_tot
    pct = (delta_tot / wo_tot * 100.0) if wo_tot else 0.0
    lines.append(
        f"| **TOTAL** | **{fmt_int(totals['with_p'])}** | **{fmt_int(totals['with_c'])}** | — | "
        f"**{totals['with_w']}s** | **{fmt_int(totals['wo_p'])}** | **{fmt_int(totals['wo_c'])}** | — | "
        f"**{totals['wo_w']}s** | **{'+' if delta_tot >= 0 else ''}{fmt_int(delta_tot)} ({pct:+.1f}%)** |"
    )
    lines.append("")
    lines.append(
        f"**Headline:** WITH-MCP {fmt_int(w_tot)} model tokens vs WITHOUT-MCP {fmt_int(wo_tot)} — "
        f"**{'savings' if delta_tot < 0 else 'overhead'} of {abs(pct):.1f}%** "
        f"({fmt_int(abs(delta_tot))} tokens absolute)."
    )
    lines.append("")
    lines.append("Model: see per-run JSON `model` field. Self-hosted, $0 / run.")
    return "\n".join(lines)


def _get(r: dict, key: str) -> int | float:
    if key == "input":
        return r.get("usage", {}).get("input_tokens", 0)
    if key == "output":
        return r.get("usage", {}).get("output_tokens", 0)
    if key == "turns":
        return r.get("num_turns", 0)
    if key == "wall":
        return r.get("bench", {}).get("wall_secs", 0)
    return 0


def main() -> int:
    if len(sys.argv) < 2:
        print("usage: ollama_compare.py <bench_dir> [label]", file=sys.stderr)
        return 2
    bench_dir = Path(sys.argv[1])
    label = sys.argv[2] if len(sys.argv) > 2 else bench_dir.name
    by_q = aggregate(bench_dir / "results")
    if not by_q:
        print(f"no results found under {bench_dir}/results/", file=sys.stderr)
        return 1
    print(emit_table(by_q, label))
    return 0


if __name__ == "__main__":
    sys.exit(main())
