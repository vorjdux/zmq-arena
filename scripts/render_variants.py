#!/usr/bin/env python3
"""Publish variants.json to docs/, and check it covers the matrix.

`variants.json` is the single source of truth for how a measured series is
presented: its display label, its colour, and the runtime note that tells two
series of one engine apart. Everything that draws a series reads it -- the four
dashboard pages, the SVG reporter, and the result renderer.

The check matters more than the copy. Adding a variant to the matrix without
adding it here used to produce a chart series labelled with its raw key and
drawn in fallback grey, which looks like a rendering glitch rather than a
missing entry, and shipped that way twice. Now the mismatch fails a script.

Usage:
  python3 scripts/render_variants.py           # verify + publish docs/variants.json
  python3 scripts/render_variants.py --check   # verify only, no write
"""

import argparse
import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "scripts"))


def matrix_variant_keys() -> set:
    """Every variant key the generated matrix will actually produce.

    Imported from gen_matrix rather than read from matrix.linode.json so the
    check follows the generator, not a possibly stale committed artifact.
    """
    import gen_matrix
    import render_results

    return {
        render_results.variant_key(t["id"], t.get("variant"))
        for t in gen_matrix.TARGETS
    }


def main():
    ap = argparse.ArgumentParser(description="Publish and validate variants.json")
    ap.add_argument("--variants", default=REPO / "variants.json", type=Path)
    ap.add_argument("--docs", default=REPO / "docs", type=Path)
    ap.add_argument("--check", action="store_true", help="validate only, do not write")
    args = ap.parse_args()

    data = json.loads(args.variants.read_text())
    known = {v["key"] for v in data["variants"]}

    missing = sorted(matrix_variant_keys() - known)
    if missing:
        print(f"error: these variants are in the matrix but not in {args.variants.name}:",
              file=sys.stderr)
        for k in missing:
            print(f"  {k}", file=sys.stderr)
        print("\nWithout an entry they render as a raw key in fallback grey.",
              file=sys.stderr)
        return 1

    # Every variant colour must resolve, and every engine referenced must have a
    # hue, or a swatch silently falls back and two engines look alike.
    engines = data["engines"]
    for v in data["variants"]:
        for field in ("label", "color", "engine"):
            if not v.get(field):
                print(f"error: variant {v['key']} has no {field}", file=sys.stderr)
                return 1
        if v["engine"] not in engines:
            print(f"error: variant {v['key']} names engine {v['engine']}, "
                  f"which has no hue in `engines`", file=sys.stderr)
            return 1

    unused = sorted(known - matrix_variant_keys())
    live = [v for v in data["variants"] if not v.get("stub") and not v.get("retired")]
    print(f"variants.json: {len(data['variants'])} entries, {len(live)} live, "
          f"all {len(matrix_variant_keys())} matrix variants covered")
    if unused:
        print(f"  not in the matrix (stubs and retired engines): {', '.join(unused)}")

    if args.check:
        return 0

    args.docs.mkdir(parents=True, exist_ok=True)
    (args.docs / "variants.json").write_text(json.dumps(data, indent=2) + "\n")
    print(f"wrote {args.docs / 'variants.json'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
