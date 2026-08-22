#!/usr/bin/env python3
"""Publish variants.json to docs/, and check it covers the matrix.

`variants.json` is the single source of truth for how a measured series is
presented: its display label, its colour, and the runtime note that tells two
series of one engine apart. Everything that draws a series reads it -- the four
dashboard pages and the result renderer.

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


# --panel in docs/*.html for each theme: the surface every line and swatch is
# drawn on. A hue bright enough to read on the dark panel is usually too pale on
# the light one and the other way round, so each variant carries a colour per
# theme and both are held to the same floor.
PANEL_BG = "#171b21"
LIGHT_PANEL = "#ffffff"
# WCAG 2.1 minimum for non-text graphical objects.
MIN_CONTRAST = 3.0


def _channel(v: int) -> float:
    c = v / 255
    return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4


def relative_luminance(hex_color: str) -> float:
    h = hex_color.lstrip("#")
    r, g, b = (_channel(int(h[i:i + 2], 16)) for i in (0, 2, 4))
    return 0.2126 * r + 0.7152 * g + 0.0722 * b


def contrast(a: str, b: str) -> float:
    la, lb = relative_luminance(a), relative_luminance(b)
    hi, lo = max(la, lb), min(la, lb)
    return (hi + 0.05) / (lo + 0.05)


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

    # A colour too close to the panel background is not a colour anyone can
    # match against a legend. The dark end of each engine's ramp had drifted
    # there: three variants sat under 2.5:1, and all three were the dotted ones,
    # which put the least ink on screen. A dotted near-black line is invisible,
    # so the reader falls back to guessing which line is which. Enforce the WCAG
    # 3:1 floor for graphical objects.
    for v in data["variants"]:
        c = contrast(v["color"], PANEL_BG)
        if c < MIN_CONTRAST:
            print(f"error: variant {v['key']} colour {v['color']} has contrast "
                  f"{c:.2f}:1 against the dashboard panel {PANEL_BG}, below the "
                  f"{MIN_CONTRAST}:1 floor. Pick a lighter shade of the same hue.",
                  file=sys.stderr)
            return 1

    # The light theme is a real surface, not a fallback, so its colours are held
    # to the same floor rather than being reported as debt.
    for v in data["variants"]:
        if not v.get("color_light"):
            print(f"error: variant {v['key']} has no color_light; the light theme "
                  f"would fall back to the dark palette and wash out",
                  file=sys.stderr)
            return 1
        c = contrast(v["color_light"], LIGHT_PANEL)
        if c < MIN_CONTRAST:
            print(f"error: variant {v['key']} light colour {v['color_light']} has "
                  f"contrast {c:.2f}:1 against the light panel {LIGHT_PANEL}, below "
                  f"the {MIN_CONTRAST}:1 floor. Pick a darker shade of the same hue.",
                  file=sys.stderr)
            return 1

    # Engine hues back the small swatches on the ranking and table pages.
    for name, hue in data["engines"].items():
        light = data.get("engines_light", {}).get(name)
        if not light:
            print(f"error: engine {name} has no entry in `engines_light`",
                  file=sys.stderr)
            return 1
        for hexv, bg, where in ((hue, PANEL_BG, "dark"), (light, LIGHT_PANEL, "light")):
            c = contrast(hexv, bg)
            if c < MIN_CONTRAST:
                print(f"error: engine {name} {where} hue {hexv} has contrast "
                      f"{c:.2f}:1 against {bg}", file=sys.stderr)
                return 1

    # An entry the matrix can never produce is dead weight: it cannot appear in a
    # chart, and it keeps a name alive after the engine is gone. Report it.
    unused = sorted(known - matrix_variant_keys())
    print(f"variants.json: {len(data['variants'])} entries, "
          f"all {len(matrix_variant_keys())} matrix variants covered")
    if unused:
        print(f"  WARNING: described but never produced by the matrix: {', '.join(unused)}",
              file=sys.stderr)

    if args.check:
        return 0

    args.docs.mkdir(parents=True, exist_ok=True)
    (args.docs / "variants.json").write_text(json.dumps(data, indent=2) + "\n")
    print(f"wrote {args.docs / 'variants.json'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
