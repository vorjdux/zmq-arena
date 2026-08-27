#!/usr/bin/env python3
"""Fill in host facts a past run did not record.

The arena samples provenance rather than asserting it: `_host.json` is written
from what the machine reported at run time. Archives written before a field
existed simply do not have it, and there is no way to recover it from the
archive itself.

This fills those gaps from the machine you run it on, which is only correct if
that machine is the one that produced the run. Two things keep that honest:

  * it refuses to write when the archive's CPU does not match this host, so a
    laptop cannot stamp its distro onto a bench-host archive;
  * it records what it filled in under `hardware.backfilled`, so a reader can
    tell an asserted value from a measured one. The dashboard marks them.

It never overwrites a value the run actually measured.

Usage:
  scripts/backfill-host.py docs/history/*-run.json            # from this host
  scripts/backfill-host.py --os "Ubuntu 24.04.4 LTS" --arch x86_64 FILE...
  scripts/backfill-host.py --dry-run FILE...                  # show, write nothing
"""
import argparse
import json
import os
import platform
import sys
from pathlib import Path

FIELDS = ("os", "arch")


def host_os() -> str | None:
    for path in ("/etc/os-release", "/usr/lib/os-release"):
        try:
            for line in Path(path).read_text().splitlines():
                if line.startswith("PRETTY_NAME="):
                    v = line.split("=", 1)[1].strip().strip('"').strip()
                    if v:
                        return v
        except OSError:
            continue
    return None


def host_cpu() -> str | None:
    try:
        for line in Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                return line.split(":", 1)[1].strip()
    except OSError:
        pass
    return None


def main() -> int:
    ap = argparse.ArgumentParser(description="Backfill host facts into run archives")
    ap.add_argument("files", nargs="+", type=Path)
    ap.add_argument("--os", dest="os_name", help="distro string; default: read this host")
    ap.add_argument("--arch", help="machine architecture; default: read this host")
    ap.add_argument("--dry-run", action="store_true", help="report, write nothing")
    ap.add_argument("--force", action="store_true",
                    help="write even when the archive's CPU is a different machine")
    a = ap.parse_args()

    values = {
        "os": a.os_name or host_os(),
        "arch": a.arch or platform.machine() or None,
    }
    missing = [k for k, v in values.items() if not v]
    if missing:
        print(f"error: no value for {', '.join(missing)}; pass it explicitly", file=sys.stderr)
        return 1

    this_cpu = host_cpu()
    print(f"filling from: os={values['os']!r} arch={values['arch']!r}")
    if this_cpu:
        print(f"this host cpu: {this_cpu!r}")
    print()

    rc = 0
    for path in a.files:
        try:
            doc = json.loads(path.read_text())
        except (OSError, ValueError) as e:
            print(f"{path}: cannot read ({e})", file=sys.stderr)
            rc = 1
            continue
        hw = doc.get("hardware")
        if not isinstance(hw, dict):
            print(f"{path}: no hardware block, skipped")
            continue

        gaps = [f for f in FIELDS if not hw.get(f)]
        if not gaps:
            print(f"{path}: already complete, nothing to do")
            continue

        archive_cpu = hw.get("cpu")
        if this_cpu and archive_cpu and archive_cpu != this_cpu and not a.force:
            print(f"{path}: REFUSED\n"
                  f"  recorded on: {archive_cpu}\n"
                  f"  this host:   {this_cpu}\n"
                  f"  Run it on the machine that produced the run, or pass --force "
                  f"if you are sure.", file=sys.stderr)
            rc = 1
            continue

        for f in gaps:
            hw[f] = values[f]
        # Asserted, not sampled. Recorded so the difference survives into the
        # dashboard rather than being lost the moment the file is written.
        marked = sorted(set(hw.get("backfilled") or []) | set(gaps))
        hw["backfilled"] = marked
        print(f"{path}: filled {', '.join(gaps)}" + (" (dry run)" if a.dry_run else ""))
        if not a.dry_run:
            tmp = path.with_suffix(path.suffix + ".tmp")
            tmp.write_text(json.dumps(doc))
            os.replace(tmp, path)
    return rc


if __name__ == "__main__":
    raise SystemExit(main())
