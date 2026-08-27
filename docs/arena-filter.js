// Shared library filter, used by every page that charts or tables variants.
//
// There are twenty series across seven languages, and the job of this bar is to
// get a reader down to the handful they can actually compare. It replaced three
// overlapping mechanisms -- facet pills, preset buttons and per-library chips --
// that each edited the same selection from a different direction, filled five
// rows before any data, and never showed what was active. A filter left on from
// an earlier visit was invisible, so libraries looked missing when they were
// only hidden.
//
// One mechanism now. Each facet is a menu of values, a value is either included
// or not, and the selection is the intersection across facets. The state is
// always spelled out in words next to the menus, and one click resets it.
(function (global) {
  "use strict";

  const KEY = "zmq-arena.filter.v2";

  // libzmq is the reference every ranking is a ratio to. It used to be pinned
  // into every selection for that reason, but the ratios are computed from the
  // full record set and only the displayed rows are filtered, so hiding libzmq
  // costs nothing: someone comparing "just the Java ones" can keep the C++
  // filter on to see the baseline, or drop it to read the Java rows alone.
  // It is still tagged, because knowing what the ratios are against matters.
  const BASELINE = "libzmq";

  const FACETS = [
    { key: "language", label: "Language" },
    { key: "family", label: "Family",
      hint: "the project, so a binding sits with the engine it binds" },
    { key: "impl", label: "Kind", rename: { native: "implementation", ffi: "binding" },
      hint: "an implementation of the protocol, or a binding to someone else's" },
    { key: "io", label: "IO" },
    { key: "threading", label: "Threading" },
    { key: "concurrency", label: "API" },
  ];

  const isBaseline = (key) => key === BASELINE;

  // State is the set of excluded facet values, not the resulting library list.
  // Storing the intent rather than its result means a stored filter still means
  // the same thing after a run adds libraries, instead of silently hiding them.
  function loadState() {
    try {
      const raw = JSON.parse(localStorage.getItem(KEY));
      if (raw && typeof raw === "object") return raw;
    } catch (e) { /* unavailable or corrupt storage: start unfiltered */ }
    return {};
  }

  function saveState(state) {
    try { localStorage.setItem(KEY, JSON.stringify(state)); } catch (e) { /* private mode */ }
  }

  const excluded = (state, facet, value) => !!(state[facet] && state[facet][value]);

  /// The libraries a state admits: every facet must accept them.
  function resolve(state, meta, all) {
    const keep = all.filter((k) => {
      const m = meta[k] || {};
      return FACETS.every((f) => !excluded(state, f.key, m[f.key]));
    });
    return new Set(keep);
  }

  function values(meta, all, facet) {
    const seen = new Map();
    for (const k of all) {
      const v = (meta[k] || {})[facet];
      if (v == null) continue;
      seen.set(v, (seen.get(v) || 0) + 1);
    }
    return [...seen.entries()].sort((a, b) => b[1] - a[1] || String(a[0]).localeCompare(b[0]));
  }

  function label(f, v) { return (f.rename && f.rename[v]) || v; }

  function renderFilters(host, meta, all, _selected, onChange) {
    if (!host) return;
    const state = loadState();
    const commit = () => { saveState(state); onChange(resolve(state, meta, all)); };

    host.innerHTML = "";
    const bar = document.createElement("div");
    bar.className = "fbar";
    host.appendChild(bar);

    const tag = document.createElement("span");
    tag.className = "fbar-label";
    tag.textContent = "Filter";
    bar.appendChild(tag);

    for (const f of FACETS) {
      const vals = values(meta, all, f.key);
      // A facet with one value cannot narrow anything.
      if (vals.length < 2) continue;
      const off = vals.filter(([v]) => excluded(state, f.key, v)).length;

      const menu = document.createElement("details");
      menu.className = "fmenu" + (off ? " active" : "");
      const sum = document.createElement("summary");
      sum.innerHTML = `${f.label}${off ? ` <b>${vals.length - off}/${vals.length}</b>` : ""}`;
      if (f.hint) sum.title = f.hint;
      menu.appendChild(sum);

      const body = document.createElement("div");
      body.className = "fmenu-body";
      for (const [v, n] of vals) {
        const on = !excluded(state, f.key, v);
        const row = document.createElement("label");
        row.className = "fitem";
        row.innerHTML = `<input type="checkbox"${on ? " checked" : ""}>` +
          `<span class="fv">${label(f, v)}</span><span class="fn">${n}</span>`;
        row.querySelector("input").addEventListener("change", (ev) => {
          state[f.key] = state[f.key] || {};
          if (ev.target.checked) delete state[f.key][v];
          else state[f.key][v] = true;
          if (!Object.keys(state[f.key]).length) delete state[f.key];
          commit();
        });
        // "only" is the common case -- show me just the Java ones -- and doing
        // it by unchecking five boxes is the kind of friction that makes a
        // filter feel broken.
        const only = document.createElement("button");
        only.className = "fonly";
        only.textContent = "only";
        only.title = `show only ${label(f, v)}`;
        only.addEventListener("click", (ev) => {
          ev.preventDefault();
          state[f.key] = {};
          vals.forEach(([other]) => { if (other !== v) state[f.key][other] = true; });
          commit();
        });
        row.appendChild(only);
        body.appendChild(row);
      }
      menu.appendChild(body);
      bar.appendChild(menu);
    }

    // What is actually showing, in words. The filter that caused trouble was the
    // one nobody could see.
    const sel = resolve(state, meta, all);
    const active = FACETS.filter((f) => state[f.key]).map((f) => {
      const kept = values(meta, all, f.key)
        .filter(([v]) => !excluded(state, f.key, v)).map(([v]) => label(f, v));
      return `${f.label}: ${kept.join(", ") || "none"}`;
    });
    const status = document.createElement("span");
    status.className = "fstatus" + (active.length ? " on" : "");
    status.textContent = active.length
      ? `${sel.size} of ${all.length} libraries · ${active.join(" · ")}`
      : `all ${all.length} libraries`;
    bar.appendChild(status);

    if (active.length) {
      const reset = document.createElement("button");
      reset.className = "freset";
      reset.textContent = "reset";
      reset.addEventListener("click", () => {
        for (const k of Object.keys(state)) delete state[k];
        commit();
      });
      bar.appendChild(reset);
    }

    // One menu open at a time, and clicking away closes it.
    bar.querySelectorAll("details").forEach((d) => {
      d.addEventListener("toggle", () => {
        if (d.open) bar.querySelectorAll("details").forEach((o) => { if (o !== d) o.open = false; });
      });
    });
    if (!host.__outside) {
      host.__outside = true;
      document.addEventListener("click", (ev) => {
        if (!host.contains(ev.target)) host.querySelectorAll("details[open]").forEach((d) => { d.open = false; });
      });
    }
  }

  /// The selection a stored filter implies, for a page that is just loading.
  function load(all, meta) {
    return meta ? resolve(loadState(), meta, all) : new Set(all);
  }

  // Kept so a page can still toggle one library without going through a facet.
  function save() { /* selection is derived from the filter; nothing to store */ }

  global.ArenaFilter = { load, save, renderFilters, isBaseline, BASELINE, FACETS };
})(window);
