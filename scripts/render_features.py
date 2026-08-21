#!/usr/bin/env python3
"""Render features.json into FEATURES.md.

zmq-arena is not only about speed. Which socket types and transports an
implementation actually has, whether its CURVE works, whether you can use it
without adopting an async runtime -- those decide adoption at least as often as
a throughput number does, and no benchmark reports them.

This is the one part of the repo that is curated rather than measured, so the
output says so at the top and every row carries its source. Nothing here is
inferred from a benchmark result.

Usage:
  python3 scripts/render_features.py            # writes FEATURES.md
"""

import argparse
import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "scripts"))


def measured_kinds() -> dict:
    """What the matrix actually schedules per implementation.

    Derived rather than curated. `arena_tier` used to be a hand-written string
    and drifted the moment the tiering changed: three implementations still
    claimed "headline" long after they had started running the extended cells
    too. A claim about our own matrix has no business being typed by hand when
    the matrix is right there.
    """
    import gen_matrix

    out = {}
    for t in gen_matrix.TARGETS:
        out.setdefault(t["id"], set()).update(t["kinds"])
    return out


def tier_of(impl_id: str, kinds: dict) -> str:
    import gen_matrix

    got = kinds.get(impl_id)
    if not got:
        return "not benchmarked"
    headline = sorted(got & set(gen_matrix.HEADLINE_KINDS))
    extended = got & (set(gen_matrix.EXTENDED_KINDS) | set(gen_matrix.IPC_KINDS))
    if len(headline) == len(gen_matrix.HEADLINE_KINDS) and extended:
        return "headline + extended"
    parts = []
    if headline:
        parts.append("headline: " + ", ".join(headline))
    if extended:
        parts.append("extended")
    return "; ".join(parts) or "not benchmarked"

MARK = {"yes": "yes", "declared": "declared", "no": "no", "partial": "partial"}


def mark(v: str) -> str:
    """Render a capability value, keeping 'declared' visibly distinct from 'yes'.

    The distinction is the whole point: 'yes' means the arena has exercised it,
    'declared' means we are repeating the project's claim.
    """
    return MARK.get(v, v)


def table(impls: list) -> str:
    rows = [
        "| capability | " + " | ".join(i["label"] for i in impls) + " |",
        "|---|" + "|".join("---" for _ in impls) + "|",
    ]

    def row(label, fn):
        rows.append(f"| {label} | " + " | ".join(fn(i) for i in impls) + " |")

    row("version", lambda i: i["version"])
    row("language", lambda i: i["language"])
    row("implementation", lambda i: "native" if i["impl"] == "native" else "FFI to libzmq")
    row("socket types", lambda i: str(len(i["socket_types"])) if not str(i["socket_types"][0]).startswith("declared") else "declared")
    row("transports", lambda i: ", ".join(i["transports"]))
    row("NULL", lambda i: mark(i["security"]["null"]))
    row("PLAIN", lambda i: mark(i["security"]["plain"]))
    row("CURVE", lambda i: mark(i["security"]["curve"]))
    row("usable without an async runtime", lambda i: mark(i["runtime_free"]))
    row("platforms", lambda i: ", ".join(i["platforms"]))
    row("bindings", lambda i: i["bindings"])
    row("benchmarked here", lambda i: i["arena_tier"])
    return "\n".join(rows)


def main():
    ap = argparse.ArgumentParser(description="Render features.json into FEATURES.md")
    ap.add_argument("--features", default=REPO / "features.json", type=Path)
    ap.add_argument("--out", default=REPO / "FEATURES.md", type=Path)
    ap.add_argument("--docs", default=REPO / "docs", type=Path,
                    help="also publish features.json here for the dashboard page")
    args = ap.parse_args()

    data = json.loads(args.features.read_text())
    impls = data["implementations"]

    # Overwrite whatever the file claims: the matrix is the authority on what
    # this repo measures.
    kinds = measured_kinds()
    for i in impls:
        i["arena_tier"] = tier_of(i["id"], kinds)

    parts = [
        "# Feature matrix\n",
        "What each implementation supports, next to what it measures. A fast library "
        "that lacks the socket type you need, or whose CURVE does not interoperate, is "
        "not a candidate no matter where it lands on a chart.\n",
        "> **This page is curated, not measured.** Every other number in this repo comes "
        "out of a run. These rows come from each project's own documentation, read on "
        f"{data['verified_on']}. `yes` means the project documents it *and* zmq-arena "
        "exercises it; **`declared` means we are repeating the project's claim and have "
        "not tested it**. Two of the implementations below are describe-only stubs, so "
        "every capability they list is unverified.\n",
        "Regenerate with `python3 scripts/render_features.py` after editing "
        "`features.json`.\n",
        "## Matrix\n",
        table(impls),
        "\n## Notes\n",
    ]

    for i in impls:
        parts.append(f"### {i['label']} {i['version']}\n")
        parts.append(f"- Socket types: {', '.join(i['socket_types'])}")
        parts.append(f"- Runtime: {i['runtime_note']}")
        if i.get("notes"):
            parts.append(f"- {i['notes']}")
        parts.append(f"- Source: {i['source']}\n")

    args.out.write_text("\n".join(parts) + "\n")

    # The dashboard is served from docs/ and cannot fetch above its own root, so
    # the curated source is copied in rather than duplicated by hand.
    if args.docs:
        args.docs.mkdir(parents=True, exist_ok=True)
        (args.docs / "features.json").write_text(json.dumps(data, indent=2) + "\n")

    print(f"wrote {args.out} and {args.docs / 'features.json'} ({len(impls)} implementations)")


if __name__ == "__main__":
    main()
