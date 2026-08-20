#!/usr/bin/env python3
"""Carry the published run history forward into a fresh checkout.

Why this exists. The site is deployed as a GitHub Pages *artifact* built from
the workspace, and `docs/history/` is gitignored so weekly runs never churn the
repository. Those two facts together mean a fresh CI checkout starts with no
archives at all, and a naive deploy would publish a site containing only the run
that just finished -- which would silently destroy the dashboard's "Evolution
over time" view, since that needs several runs to plot.

So before building the artifact, we download what is already live and put it
back. The published site is the store: there is no extra branch to maintain and
main stays code-only.

Growth is bounded here rather than left to accumulate: --keep drops the oldest
archives so the deployed artifact stays a fixed size no matter how many years of
weekly runs go by. Dropped runs are gone from the site, so keep a generous window
and keep the CI artifact backup if a longer record ever matters.

Usage:
  python3 scripts/sync_history.py --base-url https://vorjdux.github.io/zmq-arena
  python3 scripts/sync_history.py --base-url ... --keep 26 --docs docs
"""

import argparse
import json
import sys
import urllib.error
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
TIMEOUT = 30


def fetch(url: str) -> bytes | None:
    """GET a URL, returning None for 404. A missing manifest is the normal state
    on the very first deploy, not an error, so it must not fail the run."""
    try:
        with urllib.request.urlopen(url, timeout=TIMEOUT) as r:
            return r.read()
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return None
        raise
    except urllib.error.URLError as e:
        print(f"  warning: {url} unreachable ({e.reason}); treating as empty", file=sys.stderr)
        return None


def main():
    ap = argparse.ArgumentParser(description="Restore published run archives into docs/history")
    ap.add_argument("--base-url", required=True,
                    help="root of the published site, e.g. https://user.github.io/zmq-arena")
    ap.add_argument("--docs", default=REPO / "docs", type=Path)
    ap.add_argument("--keep", type=int, default=26,
                    help="most recent runs to keep on the site (default 26, about six months of weekly runs)")
    args = ap.parse_args()

    base = args.base_url.rstrip("/")
    hist = args.docs / "history"
    hist.mkdir(parents=True, exist_ok=True)

    raw = fetch(f"{base}/history/index.json")
    if raw is None:
        print("no published history yet (first deploy); nothing to restore")
        published = []
    else:
        published = json.loads(raw).get("runs", [])
        print(f"published manifest lists {len(published)} run(s)")

    # Anything already on disk is from the run that just finished and always wins:
    # never overwrite fresh output with a stale copy of the same date.
    local = {p.name for p in hist.glob("*-run.json")}
    restored = 0
    for run in published:
        fname = run.get("file")
        if not fname or fname in local:
            continue
        blob = fetch(f"{base}/history/{fname}")
        if blob is None:
            print(f"  warning: manifest lists {fname} but it is not published; skipping", file=sys.stderr)
            continue
        (hist / fname).write_bytes(blob)
        restored += 1
    print(f"restored {restored} archive(s) from the live site")

    # Rebuild the manifest from what is actually on disk, so a half-restored
    # download can never leave the manifest pointing at a file the site lacks.
    runs = sorted(p.name for p in hist.glob("*-run.json"))
    dropped = []
    if args.keep > 0 and len(runs) > args.keep:
        dropped, runs = runs[: -args.keep], runs[-args.keep:]
        for name in dropped:
            (hist / name).unlink()

    manifest = {
        "schema": 1,
        "sample": False,
        "note": (
            "Real grid runs. Rebuilt on every deploy by scripts/sync_history.py from "
            "the archives present in this directory."
        ),
        "runs": [{"date": name[: -len("-run.json")], "file": name} for name in runs],
    }
    (hist / "index.json").write_text(json.dumps(manifest, indent=2) + "\n")

    if dropped:
        print(f"pruned {len(dropped)} run(s) beyond --keep {args.keep}: {', '.join(dropped)}")
    print(f"history now holds {len(runs)} run(s) -> {hist / 'index.json'}")


if __name__ == "__main__":
    main()
