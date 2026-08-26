// Shared library selection and filtering, used by every page that charts or
// tables variants.
//
// Two things live here. First, the selection is persisted and shared: with
// twenty series across seven languages, re-picking the same handful on each page
// was most of the work of using the dashboard. Pick once, and Overview,
// Rankings, Explore and Tables all show that set. Second, the filters answer
// "which of these are even comparable" -- only the Java ones, or Python and
// Ruby, or every variant of monocoque and omq side by side.
(function (global) {
  "use strict";

  const KEY = "zmq-arena.selection.v1";

  // Facets a reader actually groups by. `impl` is the one that keeps a language
  // binding from being mistaken for a competing implementation: pyzmq is
  // libzmq underneath, so it answers what Python costs rather than how good a
  // ZMTP implementation it is.
  const FACETS = [
    { key: "language", label: "Language" },
    { key: "engine", label: "Engine" },
    { key: "impl", label: "Kind", rename: { native: "implementation", ffi: "binding" } },
  ];

  function load(all) {
    try {
      const raw = JSON.parse(localStorage.getItem(KEY));
      if (Array.isArray(raw) && raw.length) {
        // Drop keys that no longer exist, so a stored selection from an older
        // run does not silently hide variants added since.
        const kept = raw.filter((k) => all.includes(k));
        if (kept.length) return new Set(kept);
      }
    } catch (e) { /* corrupt or unavailable storage: fall back to everything */ }
    return new Set(all);
  }

  function save(selected) {
    try {
      localStorage.setItem(KEY, JSON.stringify([...selected]));
    } catch (e) { /* private mode, quota: selection just does not persist */ }
  }

  function values(meta, all, facet) {
    const seen = new Map();
    for (const k of all) {
      const v = (meta[k] || {})[facet];
      if (!v) continue;
      seen.set(v, (seen.get(v) || 0) + 1);
    }
    return [...seen.entries()].sort((a, b) => b[1] - a[1] || String(a[0]).localeCompare(b[0]));
  }

  /// Render the facet bar into `host`. `onChange` receives the new selection.
  function renderFilters(host, meta, all, selected, onChange) {
    if (!host) return;
    const apply = (next) => { save(next); onChange(next); };
    host.innerHTML = "";

    for (const f of FACETS) {
      const vals = values(meta, all, f.key);
      // A facet with one value tells the reader nothing and costs a row.
      if (vals.length < 2) continue;
      const row = document.createElement("div");
      row.className = "facet";
      row.innerHTML = `<span class="fname">${f.label}</span>`;
      for (const [val, n] of vals) {
        const members = all.filter((k) => (meta[k] || {})[f.key] === val);
        const on = members.every((k) => selected.has(k));
        const some = !on && members.some((k) => selected.has(k));
        const b = document.createElement("button");
        b.className = "fbtn" + (on ? " on" : some ? " some" : "");
        b.textContent = `${(f.rename && f.rename[val]) || val} ${n}`;
        b.title = on ? `hide the ${n} ${val} series` : `show the ${n} ${val} series`;
        b.addEventListener("click", (ev) => {
          const next = new Set(selected);
          // Plain click isolates the group, which is what "just the Java ones"
          // means. Shift-click adds it, for "Python and Ruby" or "monocoque and
          // omq" without starting over.
          if (!ev.shiftKey) next.clear();
          if (on && ev.shiftKey) members.forEach((k) => next.delete(k));
          else members.forEach((k) => next.add(k));
          if (!next.size) all.forEach((k) => next.add(k));
          apply(next);
        });
        row.appendChild(b);
      }
      host.appendChild(row);
    }

    const row = document.createElement("div");
    row.className = "facet";
    row.innerHTML = `<span class="fname">All</span>`;
    for (const [label, fn] of [["show all", () => new Set(all)], ["clear", () => new Set()]]) {
      const b = document.createElement("button");
      b.className = "fbtn";
      b.textContent = label;
      b.addEventListener("click", () => apply(fn()));
      row.appendChild(b);
    }
    const hint = document.createElement("span");
    hint.className = "fhint";
    hint.textContent = "click a group to show only it, shift-click to add · shared across pages";
    row.appendChild(hint);
    host.appendChild(row);
  }

  global.ArenaFilter = { load, save, renderFilters, FACETS };
})(window);
