# Working in this repo

Notes for anyone (human or agent) making changes here. The rules exist because
this is a benchmark: a change that would be harmless in an application can
silently invalidate every number the repo publishes.

## The one rule

**Never make a measurement look better than it is.** If a number is from a dev
host, say so next to the number. If a capability is a project's claim rather
than something the arena exercised, mark it `declared`. If a cell did not
converge across replicates, print the spread. Deleting a caveat is a bigger
regression than a slow socket loop.

## Layout

```
orchestrator/   control plane: cgroups, spawn isolation, telemetry, replication
reporter/       static SVG charts (plotters) from a run archive
targets/        one standalone project per implementation, NOT workspace members
scripts/        matrix generation, result rendering, feature matrix rendering
docs/           dashboard (5 pages, no build step) + generated charts
features.json   curated capability matrix, the source for FEATURES.md
variants.json   how each measured series is labelled and coloured
```

`orchestrator/` and `reporter/` are the only Cargo workspace members. Targets are
deliberately outside it: a shared workspace would resolve one dependency graph,
one release profile and one toolchain across every implementation, which
benchmarks the resolver's choices rather than what each library ships.

## Changing a target

- A target's socket loop is the implementation's own business. Tuning it is
  welcome; the protocol rules in `targets/README.md` (no dropped data, a real
  serialization round-trip) are not negotiable, and the validator enforces them.
- Tune against the engine's own benchmark peer where one exists, and say in a
  comment which one and why. An untuned wrapper measures the defaults, not the
  library. A wrapper tuned with knobs nobody would use in production measures
  nothing at all.
- After changing a target, run all five kinds over both transports by hand
  before trusting a grid run. A wrapper that compiles can still deadlock at a
  handshake.
- Bumping an engine version means re-reading its changelog for API and default
  changes. Buffer sizing in particular is not portable across versions: what is
  a tuning win in one release can be below the new default in the next.

## Changing the matrix

**Every runtime an engine ships is its own variant.** monocoque builds on
compio/tokio/smol, zmq.rs on tokio/async-std/async-dispatcher, omq runs
current-thread, multi-thread and its synchronous blocking API. If an engine adds
a runtime, add the variant; never benchmark a subset, because choosing which of
an engine's configurations may represent it decides the result before any
measurement happens. Engines that pick the runtime at compile time are built once
per runtime into separate target dirs; engines that pick at run time take a
`--variant`.

`scripts/gen_matrix.py` is the source; `matrix.linode.json` is generated output,
so edit the script and regenerate. The matrix is tiered by PATTERN (headline =
the three patterns every implementation here implements; extended = ipc and the
fan patterns), never by library. A target's `kinds` list is a capability
declaration and is the only thing that decides which cells it runs. Never shorten
it to save grid time, and never add a per-library tier flag: this is a neutral
comparator, and a library that runs extra cells because someone put it on a list
would distort every ratio computed against the baseline.

## Changing the reporting

- `scripts/render_results.py` turns a scratch directory into the archive and
  `RANKING.md`. It does data transformation only, never measurement.
- `reporter/` draws the charts. It must refuse to plot a cell that replication
  flagged `inverted`: a line through a known-wrong point reads as a real dip.
- `docs/*.html` are self-contained and dependency-free. Keep them that way; the
  dashboard has to work from a plain static file server.
- `features.json` is curated. Every row carries its source, and anything the
  arena has not exercised is `declared`, not `yes`.
- `variants.json` is the ONLY place a series' label, colour and legend order
  live. The dashboard pages fetch it, the SVG reporter bakes it in, and
  `render_results.py` reads its category tags. Never add a label or colour
  literal to a page or to the reporter: those facts were duplicated in six
  places once, and new variants shipped rendering as raw keys in fallback grey
  twice before it was noticed. `scripts/render_variants.py` fails if the matrix
  contains a variant the file does not describe, so run it after touching the
  roster.

## Conventions

- Comments explain why, not what. A comment that restates the line above it is
  noise; a comment recording the measurement that justified a constant is the
  most valuable thing in the file.
- Plain ASCII in commit messages, no em dashes, no attribution trailers.
- `make build` then `make dry` is the fast check that the wiring still works.
