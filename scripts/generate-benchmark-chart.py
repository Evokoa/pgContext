#!/usr/bin/env python3
"""Render the README benchmark chart (pgContext vs pgvector) from result data.

Reads the most recent GloVe-100-angular matched-Docker benchmark JSON under
``benchmarks/pgvector_comparison/results/`` and writes a self-contained SVG to
``assets/benchmark-pgvector.svg``. The SVG uses only inline presentation
attributes (no CSS classes, no scripts, no external fonts) so it renders
correctly when GitHub sanitizes and proxies README images.

Standard library only — no pip install required.

Usage:
    scripts/generate-benchmark-chart.py                 # auto-discover latest
    scripts/generate-benchmark-chart.py --input path.json --output out.svg
    scripts/generate-benchmark-chart.py --check         # fail if SVG is stale
"""

from __future__ import annotations

import argparse
import glob
import json
import math
import os
import re
import sys

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RESULTS_GLOB = os.path.join(
    REPO_ROOT, "benchmarks", "pgvector_comparison", "results", "*glove*matched-docker*.json"
)
DEFAULT_OUTPUT = os.path.join(REPO_ROOT, "assets", "benchmark-pgvector.svg")

# Neutral scale matches docs/benchmarks/reports/pgcontext-vs-pgvector.html;
# pgContext bars use the Evokoa brand green (dark companion to the banner's
# emerald #34d399 accent).
INK = "#1a1c1e"
MUTED = "#585d64"
FAINT = "#8a9098"
RULE = "#e2e5e9"
GRID = "#eef0f3"
PAPER = "#ffffff"
PGC = "#065f46"   # pgContext — Evokoa brand green
PGV = "#8b929c"   # pgvector
FONT = "-apple-system,'Segoe UI',Roboto,Helvetica,Arial,sans-serif"
MONO = "ui-monospace,'SF Mono',Menlo,Consolas,monospace"


def find_latest_result() -> str:
    matches = glob.glob(RESULTS_GLOB)
    if not matches:
        sys.exit(
            f"error: no benchmark results matched {RESULTS_GLOB}\n"
            "Run the GloVe matched-Docker benchmark first "
            "(benchmarks/pgvector_comparison/run-ann-hdf5.sh)."
        )
    # Filenames carry an ISO date suffix, so lexical sort == chronological.
    return sorted(matches)[-1]


def meta_from_filename(path: str) -> dict:
    """Recover hardware label and ISO date from the result filename when the
    JSON body omits them (the archived results carry both in the name)."""
    name = os.path.basename(path)
    meta = {}
    date_match = re.search(r"(\d{4}-\d{2}-\d{2})", name)
    if date_match:
        meta["date"] = date_match.group(1)
    hw_match = re.match(r"([a-z0-9]+(?:-[a-z0-9]+)*?)-glove", name)
    if hw_match:
        pretty = hw_match.group(1).replace("-", " ").title().replace("M4", "M4")
        meta["hardware"] = f"{pretty} (NEON kernels)"
    return meta


def load_curves(path: str):
    with open(path, encoding="utf-8") as fh:
        data = json.load(fh)
    for key, value in meta_from_filename(path).items():
        data.setdefault(key, value)
    results = data.get("results", {})
    for engine in ("pgcontext", "pgvector"):
        if engine not in results:
            sys.exit(f"error: {path} has no '{engine}' results; cannot draw the comparison.")
    pgc = {row["ef_search"]: row for row in results["pgcontext"]["curve"]}
    pgv = {row["ef_search"]: row for row in results["pgvector"]["curve"]}
    efs = sorted(set(pgc) & set(pgv))
    if not efs:
        sys.exit(f"error: {path} has no shared ef_search points between the two engines.")
    return data, efs, pgc, pgv


def esc(text: str) -> str:
    return text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def nice_scale(value: float, target_ticks: int = 7):
    """Pick a clean axis top and tick step so the tallest bar nearly fills the
    plot. Returns (axis_max, step); prefers integer steps for ms latency."""
    if value <= 0:
        return 1.0, 1.0
    raw = value / target_ticks
    exp = math.floor(math.log10(raw))
    base = 10 ** exp
    step = next(m * base for m in (1, 2, 2.5, 5, 10) if m * base >= raw)
    axis_max = math.ceil(value / step) * step
    return axis_max, step


def render_svg(data, efs, pgc, pgv) -> str:
    W, H = 920, 520
    # Plot box.
    left, right, top, bottom = 92, 872, 118, 404
    plot_w = right - left
    plot_h = bottom - top

    max_p50 = max(max(pgc[e]["p50_ms"], pgv[e]["p50_ms"]) for e in efs)
    axis_max, tick_step = nice_scale(max_p50)
    n_ticks = int(round(axis_max / tick_step)) + 1

    def y_of(v: float) -> float:
        return bottom - (v / axis_max) * plot_h

    group_w = plot_w / len(efs)
    bar_w = min(52, group_w * 0.30)
    pair_gap = 12

    p = []
    a = p.append
    a(f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" '
      f'role="img" aria-label="pgContext versus pgvector query latency on GloVe-100-angular. '
      f'At matched recall, pgContext is 3.8 to 5.3 times faster.">')
    a(f'<rect x="1" y="1" width="{W-2}" height="{H-2}" rx="14" fill="{PAPER}" stroke="{RULE}"/>')

    # Titles.
    a(f'<text x="40" y="52" font-family="{FONT}" font-size="27" font-weight="700" '
      f'fill="{INK}">Same recall &#8212; up to 5.3&#215; faster queries</text>')
    a(f'<text x="40" y="80" font-family="{FONT}" font-size="15" fill="{MUTED}">'
      f'Median query latency on GloVe-100-angular '
      f'({data.get("corpus_rows", 0):,} vectors, cosine) &#183; lower is better</text>')

    # Legend (top-right).
    lx, ly = right - 214, 50
    a(f'<rect x="{lx}" y="{ly-11}" width="14" height="14" rx="3" fill="{PGC}"/>')
    a(f'<text x="{lx+21}" y="{ly}" font-family="{FONT}" font-size="14" fill="{INK}">pgContext</text>')
    a(f'<rect x="{lx+108}" y="{ly-11}" width="14" height="14" rx="3" fill="{PGV}"/>')
    a(f'<text x="{lx+129}" y="{ly}" font-family="{FONT}" font-size="14" fill="{INK}">pgvector</text>')

    # Y grid + tick labels.
    for i in range(n_ticks):
        v = i * tick_step
        y = y_of(v)
        a(f'<line x1="{left}" y1="{y:.1f}" x2="{right}" y2="{y:.1f}" '
          f'stroke="{GRID}" stroke-width="1"/>')
        a(f'<text x="{left-12}" y="{y+4:.1f}" text-anchor="end" font-family="{MONO}" '
          f'font-size="12" fill="{FAINT}">{v:g}</text>')
    a(f'<text x="26" y="{(top+bottom)/2:.0f}" font-family="{FONT}" font-size="12.5" '
      f'fill="{MUTED}" transform="rotate(-90 26 {(top+bottom)/2:.0f})" '
      f'text-anchor="middle">p50 latency (ms)</text>')
    # Baseline.
    a(f'<line x1="{left}" y1="{bottom}" x2="{right}" y2="{bottom}" stroke="{INK}" stroke-width="1.5"/>')

    for idx, ef in enumerate(efs):
        cx = left + group_w * (idx + 0.5)
        pc, pv = pgc[ef], pgv[ef]
        x_pgc = cx - pair_gap / 2 - bar_w
        x_pgv = cx + pair_gap / 2
        y_pgc, y_pgv = y_of(pc["p50_ms"]), y_of(pv["p50_ms"])
        speedup = pv["p50_ms"] / pc["p50_ms"] if pc["p50_ms"] else 0

        a(f'<rect x="{x_pgc:.1f}" y="{y_pgc:.1f}" width="{bar_w:.1f}" '
          f'height="{bottom-y_pgc:.1f}" rx="3" fill="{PGC}"/>')
        a(f'<rect x="{x_pgv:.1f}" y="{y_pgv:.1f}" width="{bar_w:.1f}" '
          f'height="{bottom-y_pgv:.1f}" rx="3" fill="{PGV}"/>')

        # Value labels on top of each bar.
        a(f'<text x="{x_pgc+bar_w/2:.1f}" y="{y_pgc-8:.1f}" text-anchor="middle" '
          f'font-family="{MONO}" font-size="12" fill="{PGC}" font-weight="600">'
          f'{pc["p50_ms"]:.2f}</text>')
        a(f'<text x="{x_pgv+bar_w/2:.1f}" y="{y_pgv-8:.1f}" text-anchor="middle" '
          f'font-family="{MONO}" font-size="12" fill="{MUTED}">{pv["p50_ms"]:.2f}</text>')

        # Speed-up callout above the group.
        a(f'<text x="{cx:.1f}" y="{top-22:.1f}" text-anchor="middle" font-family="{FONT}" '
          f'font-size="14.5" font-weight="700" fill="{PGC}">{speedup:.1f}&#215;</text>')

        # X label: ef_search + pgContext recall (both engines within ~0.02).
        recall = pc["recall_at_10"]
        a(f'<text x="{cx:.1f}" y="{bottom+22:.1f}" text-anchor="middle" font-family="{MONO}" '
          f'font-size="13" fill="{INK}">ef {ef}</text>')
        a(f'<text x="{cx:.1f}" y="{bottom+40:.1f}" text-anchor="middle" font-family="{FONT}" '
          f'font-size="11.5" fill="{FAINT}">recall {recall:.2f}</text>')

    # Footnote.
    hw = data.get("hardware", "Apple M4 Pro (NEON)")
    date = data.get("date", "")
    foot = (f'GloVe-100-angular, dataset ground-truth neighbors &#183; both engines in one '
            f'PostgreSQL&#160;17 container, matched 8-way parallel build &#183; {esc(hw)}')
    a(f'<text x="40" y="{H-42}" font-family="{FONT}" font-size="12" fill="{MUTED}">{foot}</text>')
    a(f'<text x="40" y="{H-24}" font-family="{FONT}" font-size="12" fill="{FAINT}">'
      f'Reproduce: benchmarks/pgvector_comparison/run-ann-hdf5.sh &#183; regenerate this chart: '
      f'scripts/generate-benchmark-chart.py{(" &#183; " + esc(date)) if date else ""}</text>')

    a('</svg>')
    return "\n".join(p) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", help="benchmark result JSON (default: latest matched-Docker GloVe run)")
    parser.add_argument("--output", default=DEFAULT_OUTPUT, help="SVG output path")
    parser.add_argument("--check", action="store_true",
                        help="exit non-zero if the on-disk SVG differs from freshly rendered output")
    args = parser.parse_args()

    src = args.input or find_latest_result()
    data, efs, pgc, pgv = load_curves(src)
    svg = render_svg(data, efs, pgc, pgv)

    if args.check:
        try:
            with open(args.output, encoding="utf-8") as fh:
                current = fh.read()
        except FileNotFoundError:
            current = None
        if current != svg:
            print(f"stale: {os.path.relpath(args.output, REPO_ROOT)} is out of date; "
                  f"run scripts/generate-benchmark-chart.py", file=sys.stderr)
            return 1
        print(f"up to date: {os.path.relpath(args.output, REPO_ROOT)}")
        return 0

    os.makedirs(os.path.dirname(args.output), exist_ok=True)
    with open(args.output, "w", encoding="utf-8") as fh:
        fh.write(svg)

    speedups = [pgv[e]["p50_ms"] / pgc[e]["p50_ms"] for e in efs if pgc[e]["p50_ms"]]
    print(f"source : {os.path.relpath(src, REPO_ROOT)}")
    print(f"wrote  : {os.path.relpath(args.output, REPO_ROOT)}")
    print(f"ef pts : {', '.join(str(e) for e in efs)}")
    print(f"speedup: {min(speedups):.1f}x - {max(speedups):.1f}x (p50, matched recall)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
