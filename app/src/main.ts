import { control, inTauri, invoke, listen } from "./api";
import { DeckView } from "./deck";
import { computeLayout, CosmosView, type GraphView, SigmaView } from "./graph";
import { installKeyboard, keymapHelp, type KeyBinding } from "./keys";
import { fromLibrary, search, syntheticLibrary } from "./library";
import { FrameStats, gpuInfo } from "./perf";
import { compatibilityEdges, effectiveKey, type Reference, type Scored, suggest } from "./suggest";
import { FolderTree, type MenuItem, showMenu } from "./tree";
import type { AnalysisEvent, DeckGridEvent, EngineState, LibTrack, LoadedTrack, MidiEvent, OutputInfo, Track, UiControl } from "./types";

const $ = <T extends HTMLElement = HTMLElement>(sel: string) => document.querySelector<T>(sel)!;

// ---- state ---------------------------------------------------------------------------

let folders: string[] = [];
let tracks: Track[] = [];
let byId = new Map<number, Track>();
let filtered: Track[] = [];
let suggestions: Scored[] = [];
let selectedId: number | null = null;
let focusedPane: "library" | "suggest" = "library";
/** Synthetic tracks added to the graph, only while the benchmark runs. */
let synthCount = 0;
let graphNodes: Track[] = [];
let graphIndex = new Map<number, number>();
let layoutMode: "wheel" | "vibe" = "wheel";
let graphLib: "sigma" | "cosmos" = "sigma";
let energyDelta = 0;
let engine: EngineState | null = null;
const played = new Set<number>();
const playStarted: (number | null)[] = [null, null];
let lastStarted: number | null = null;
let refKey = "";

const decks = [0, 1].map((i) => new DeckView(i, $(`.deck[data-deck="${i}"]`), $(`.wave-slot[data-deck="${i}"]`), (d, id) => loadToDeck(d, id)));

// ---- virtual lists ---------------------------------------------------------------------

class VList<T> {
  private spacer = document.createElement("div");
  private pool: HTMLElement[] = [];
  items: T[] = [];
  selected = -1;

  constructor(
    private el: HTMLElement,
    private rowH: number,
    private render: (item: T, row: HTMLElement, selected: boolean) => void,
    private idOf: (item: T) => number,
    private onPick: (item: T) => void,
    private onActivate?: (item: T) => void,
    private onContext?: (item: T, e: MouseEvent) => void,
  ) {
    el.appendChild(this.spacer);
    el.addEventListener("scroll", () => this.draw());
    new ResizeObserver(() => this.draw()).observe(el);
  }

  set(items: T[], selected = -1) {
    this.items = items;
    this.selected = selected;
    this.spacer.style.height = `${items.length * this.rowH}px`;
    this.draw();
  }

  select(i: number) {
    if (!this.items.length) return;
    this.selected = Math.max(0, Math.min(this.items.length - 1, i));
    const top = this.selected * this.rowH;
    if (top < this.el.scrollTop) this.el.scrollTop = top;
    else if (top + this.rowH > this.el.scrollTop + this.el.clientHeight) this.el.scrollTop = top + this.rowH - this.el.clientHeight;
    this.draw();
    this.onPick(this.items[this.selected]);
  }

  draw() {
    const first = Math.floor(this.el.scrollTop / this.rowH);
    const count = Math.ceil(this.el.clientHeight / this.rowH) + 2;
    while (this.pool.length < count) {
      const row = document.createElement("div");
      row.draggable = true;
      row.addEventListener("click", () => this.select(+row.dataset.i!));
      row.addEventListener("dblclick", () => this.onActivate?.(this.items[+row.dataset.i!]));
      row.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        this.select(+row.dataset.i!);
        this.onContext?.(this.items[+row.dataset.i!], e);
      });
      row.addEventListener("dragstart", (e) => {
        e.dataTransfer!.setData("text/earshot-track", String(this.idOf(this.items[+row.dataset.i!])));
        e.dataTransfer!.effectAllowed = "copy";
      });
      this.el.appendChild(row);
      this.pool.push(row);
    }
    this.pool.forEach((row, k) => {
      const i = first + k;
      if (k >= count || i >= this.items.length) {
        row.style.display = "none";
        return;
      }
      row.style.display = "";
      row.style.top = `${i * this.rowH}px`;
      row.dataset.i = String(i);
      this.render(this.items[i], row, i === this.selected);
    });
  }
}

const esc = (s: string) => s.replace(/[&<>"]/g, (c) => `&#${c.charCodeAt(0)};`);

function rowClasses(row: HTMLElement, t: Track, sel: boolean, base: string) {
  const onDeck = decks.some((d) => d.track?.id === t.id && d.state?.playing);
  row.className = `row-item ${base}${sel ? " sel" : ""}${onDeck ? " playing" : ""}${played.has(t.id) ? " played" : ""}`;
}

const mmss = (s: number | null | undefined) =>
  s == null ? "" : `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, "0")}`;

function statusCell(t: Track) {
  if (t.analysis === "error") return `<span class="st error" title="${esc(t.analysisError ?? "analysis failed")}">✕</span>`;
  if (t.analysis === "running") return `<span class="st running" title="${esc(t.analysisStage ?? "Analysing")} · ${pct(t.analysisProgress ?? 0)}">●</span>`;
  if (t.analysis === "queued") return `<span class="st queued" title="Waiting to be analysed">○</span>`;
  return t.analysed ? `<span class="st done" title="Beatgrid analysed">●</span>` : `<span class="st" title="Not analysed"></span>`;
}

const libList = new VList<Track>(
  $("#lib-list"),
  26,
  (t, row, sel) => {
    rowClasses(row, t, sel, "lib-item");
    row.title = t.path ?? "";
    row.innerHTML = `${statusCell(t)}<span class="t">${esc(t.title)}</span><span class="a">${esc(t.artist)}</span>
      <span class="n${t.analysed ? "" : " tag"}" title="${t.analysed ? "Analysed" : "From the file's tag (not analysed)"}">${t.bpm?.toFixed(2) ?? "–"}</span>
      <span class="n">${t.key ?? "–"}</span><span class="n">${mmss(t.duration)}</span>${
        t.analysis === "running" ? `<i class="row-prog" style="width:${pct(t.analysisProgress ?? 0)}"></i>` : ""
      }`;
  },
  (t) => t.id,
  (t) => {
    focusPane("library");
    setSelected(t.id, "list");
  },
  (t) => loadToDeck(freeDeck(), t.id),
  (t, e) =>
    showMenu(e.clientX, e.clientY, [
      { label: "Load to deck A", action: () => loadToDeck(0, t.id) },
      { label: "Load to deck B", action: () => loadToDeck(1, t.id) },
      { label: t.analysed ? "Analyse again" : "Analyse", action: () => analyse([t.id]) },
    ]),
);

const pct = (x: number) => `${Math.round(x * 100)}%`;
const sugList = new VList<Scored>(
  $("#suggest-list"),
  40,
  (s, row, sel) => {
    rowClasses(row, s.track, sel, "sug-item");
    const ch = s.tempoChange == null ? "" : `${s.tempoChange >= 0 ? "+" : ""}${(s.tempoChange * 100).toFixed(1)}%`;
    row.innerHTML = `<span class="t">${esc(s.track.title)}<small>${esc(s.track.artist)}</small></span>
      <span class="n" title="Tempo change needed">${ch}</span><span class="n">${s.track.key ?? "–"}</span>
      <span class="score">${Math.round(s.total * 100)}</span>
      <span class="why">${[
        ["key", s.key, "#c25bff"],
        ["tempo", s.tempo, "#3fa7ff"],
        ["vibe", s.vibe, "#45d483"],
        ["energy", s.energy, "#ffcc4d"],
      ]
        .map(([n, v, c]) => `<span class="bar" title="${n} ${pct(v as number)}" style="--c:${c}"><i style="width:${pct(v as number)}"></i></span>`)
        .join("")}</span>`;
  },
  (s) => s.track.id,
  (s) => {
    focusPane("suggest");
    highlightGraph(s.track.id);
  },
);

function focusPane(p: "library" | "suggest") {
  focusedPane = p;
  $("#library").classList.toggle("focused", p === "library");
  $("#suggest").classList.toggle("focused", p === "suggest");
}
focusPane("library");

// ---- library ---------------------------------------------------------------------------

function rebuildLibrary() {
  byId = new Map(tracks.map((t) => [t.id, t]));
  applySearch();
  rebuildGraph();
}

/** Tracks in the selected folder (and its subfolders), filtered by the search box. */
function applySearch() {
  const dir = tree.selected;
  const inScope = dir == null ? tracks : tracks.filter((t) => t.path?.startsWith(`${dir}/`));
  filtered = search(inScope, $<HTMLInputElement>("#search").value);
  libList.set(filtered, filtered.findIndex((t) => t.id === selectedId));
  $("#lib-title").textContent = dir == null ? "All tracks" : dir.split("/").pop() || dir;
  $("#lib-title").title = dir ?? "";
  $("#lib-count").textContent = `${filtered.length.toLocaleString()}${filtered.length !== inScope.length ? ` of ${inScope.length.toLocaleString()}` : ""} tracks`;
  const empty = $("#lib-list").querySelector(".list-empty") ?? $("#lib-list").appendChild(Object.assign(document.createElement("div"), { className: "list-empty" }));
  empty.textContent = !folders.length ? "No library folders yet. Menu → Import → Folder…" : filtered.length ? "" : "No tracks here";
}

$<HTMLInputElement>("#search").addEventListener("input", applySearch);

const tree = new FolderTree(
  $("#tree"),
  () => applySearch(),
  (path, isRoot, e) => {
    const inside = tracks.filter((t) => t.path?.startsWith(`${path}/`));
    const items: MenuItem[] = [
      { label: `Analyse ${inside.filter((t) => !t.analysed).length} unanalysed tracks`, action: () => analyse(inside.filter((t) => !t.analysed).map((t) => t.id)) },
      { label: `Analyse all ${inside.length} tracks again`, action: () => analyse(inside.map((t) => t.id)) },
    ];
    if (isRoot) items.push({ label: "Remove from library", action: () => void removeFolder(path) });
    showMenu(e.clientX, e.clientY, items);
  },
);

async function loadLibrary() {
  $("#lib-count").textContent = "scanning…";
  try {
    const t0 = performance.now();
    const found = await invoke<LibTrack[]>("scan_library");
    tracks = found.map(fromLibrary);
    rebuildLibrary();
    console.info(`library: ${tracks.length} tracks in ${((performance.now() - t0) / 1000).toFixed(1)} s`);
  } catch (e) {
    $("#lib-count").textContent = `scan failed: ${e}`;
  }
}

async function refreshFolders(list?: string[]) {
  folders = list ?? (await invoke<string[]>("library_folders"));
  tree.setRoots(folders);
}

async function importFolder() {
  let picked: string[] = [];
  if (inTauri) {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const r = await open({ directory: true, multiple: true, title: "Add music folders to the library" });
    picked = r == null ? [] : Array.isArray(r) ? r : [r];
  } else {
    const p = prompt("Folder path");
    if (p) picked = [p];
  }
  if (!picked.length) return;
  try {
    let list = folders;
    for (const p of picked) list = await invoke<string[]>("add_library_folder", { path: p });
    await refreshFolders(list);
    await loadLibrary();
    tree.select(picked[0].replace(/\/+$/, ""));
  } catch (e) {
    alert(`Could not add folder: ${e}`);
  }
}

async function removeFolder(path: string) {
  await refreshFolders(await invoke<string[]>("remove_library_folder", { path }));
  await loadLibrary();
}

function analyse(ids: number[]) {
  if (ids.length) void invoke("analyse_tracks", { ids });
}

$("#menu-btn").addEventListener("click", (e) => {
  const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
  showMenu(r.left, r.bottom + 2, [
    { label: "Import", submenu: [{ label: "Folder…", action: () => void importFolder() }] },
    { label: "Rescan library", action: () => void loadLibrary() },
  ]);
});

// ---- analysis progress ------------------------------------------------------------------

const busy = new Set<number>();
listen<AnalysisEvent>("analysis", (ev) => {
  const t = byId.get(ev.track_id);
  if (t) {
    t.analysis = ev.state;
    t.analysisProgress = ev.progress;
    t.analysisStage = ev.stage;
    if (ev.state === "done") {
      t.analysed = true;
      if (ev.bpm != null) t.bpm = ev.bpm;
    }
    t.analysisError = ev.error;
  }
  if (ev.state === "queued" || ev.state === "running") busy.add(ev.track_id);
  else busy.delete(ev.track_id);
  const pill = $("#analysis-status");
  if (ev.state === "error") {
    pill.hidden = false;
    pill.className = "pill warn";
    pill.textContent = `analysis failed${busy.size ? ` · ${busy.size} left` : ""}`;
    pill.title = `${t?.title ?? ev.track_id}: ${ev.error}`;
  } else if (ev.state === "running") {
    pill.hidden = false;
    pill.className = "pill progress";
    pill.style.setProperty("--p", pct(ev.progress ?? 0));
    pill.textContent = `analysing ${t?.title ?? ""} · ${pct(ev.progress ?? 0)}${busy.size > 1 ? ` · ${busy.size - 1} waiting` : ""}`;
    pill.title = ev.stage ?? "";
  } else if (busy.size) {
    pill.hidden = false;
    pill.className = "pill";
    pill.textContent = `analysis · ${busy.size} waiting`;
    pill.title = "";
  } else if (!pill.classList.contains("warn")) pill.hidden = true;
  for (const d of decks) if (d.track?.id === ev.track_id) d.showAnalysis(ev);
  libList.draw();
});
$("#analysis-status").addEventListener("click", () => ($("#analysis-status").hidden = true));

// A deck's track finished analysing: show its new grid.
listen<DeckGridEvent>("deck-grid", (ev) => decks[ev.deck].setGrid(ev.grid));

/** For double-click loading: a deck that isn't playing, preferring A. */
function freeDeck() {
  return decks.findIndex((d) => !d.state?.playing) === 1 ? 1 : 0;
}

function setSelected(id: number | null, from: "list" | "graph" | "other") {
  selectedId = id;
  if (from !== "list") {
    const i = filtered.findIndex((t) => t.id === id);
    if (i >= 0) libList.select(i);
  }
  refreshSuggestions(true);
}

// ---- suggestions -----------------------------------------------------------------------

function currentReference(): Reference | null {
  // Live: the most recently started playing deck. Otherwise the selected track.
  const live = [lastStarted, 0, 1].find((d) => d != null && decks[d].state?.playing && decks[d].track) as number | undefined;
  if (live != null) {
    const d = decks[live];
    const s = d.state!;
    return {
      track: d.track!,
      bpm: s.bpm ?? d.track!.bpm,
      key: effectiveKey(d.track!.key, s.speed),
      energyDelta,
      played,
    };
  }
  const t = selectedId != null ? byId.get(selectedId) : undefined;
  if (!t) return null;
  return { track: t, bpm: t.bpm, key: effectiveKey(t.key, 1), energyDelta, played };
}

function refreshSuggestions(force = false) {
  const ref = currentReference();
  const live = ref && decks.some((d) => d.track?.id === ref.track.id && d.state?.playing);
  const key = ref ? `${ref.track.id}|${ref.bpm?.toFixed(1)}|${ref.key?.n}${ref.key?.minor}|${energyDelta}|${played.size}` : "";
  if (!force && key === refKey) return;
  refKey = key;
  if (!ref) {
    suggestions = [];
    $("#suggest-ref").textContent = "Select or play a track";
  } else {
    const t0 = performance.now();
    suggestions = suggest(ref, tracks, 80);
    const ms = performance.now() - t0;
    const deck = decks.find((d) => d.track?.id === ref.track.id && d.state?.playing);
    $("#suggest-ref").innerHTML = `${live ? `<b style="color:var(--accent)">LIVE · deck ${"AB"[deck!.index]}</b> ` : "After "}${esc(ref.track.title)} · ${ref.bpm?.toFixed(1) ?? "?"} BPM · ${ref.key ? `${ref.key.n}${ref.key.minor ? "A" : "B"}` : "?"} <span class="muted">(${ms.toFixed(1)} ms over ${tracks.length})</span>`;
  }
  sugList.set(suggestions, focusedPane === "suggest" ? Math.min(sugList.selected, suggestions.length - 1) : -1);
  highlightGraph(ref?.track.id ?? null);
}
setInterval(() => refreshSuggestions(), 500);

document.querySelectorAll<HTMLButtonElement>("#energy-mode button").forEach((b) =>
  b.addEventListener("click", () => {
    energyDelta = +b.dataset.e!;
    document.querySelectorAll("#energy-mode button").forEach((x) => x.classList.toggle("on", x === b));
    refreshSuggestions(true);
  }),
);

// ---- graph -------------------------------------------------------------------------------

let graph: GraphView | null = null;

function makeGraph() {
  graph?.destroy();
  const container = $("#graph");
  const cb = {
    onSelect: (i: number | null) => setSelected(i == null ? null : graphNodes[i]?.id ?? null, "graph"),
    onDragStart: (i: number, ev: MouseEvent) => startGhostDrag(graphNodes[i], ev),
  };
  try {
    graph = graphLib === "sigma" ? new SigmaView(container, cb) : new CosmosView(container, cb);
  } catch (e) {
    graph = null;
    $("#graph-info").textContent = `${graphLib} failed: ${e}`;
  }
}

let lastBuild = { edgesMs: 0, setDataMs: 0, edges: 0 };
function rebuildGraph() {
  if (!graph) makeGraph();
  if (!graph) return;
  graphNodes = synthCount ? [...tracks, ...syntheticLibrary(synthCount, 1_000_000)] : tracks;
  graphIndex = new Map(graphNodes.map((t, i) => [t.id, i]));
  const t0 = performance.now();
  const edges = compatibilityEdges(graphNodes);
  const t1 = performance.now();
  graph.setData(graphNodes, computeLayout(graphNodes, layoutMode), edges);
  const t2 = performance.now();
  lastBuild = { edgesMs: t1 - t0, setDataMs: t2 - t1, edges: edges.length };
  $("#graph-info").textContent = `${graphNodes.length.toLocaleString()} nodes · ${edges.length.toLocaleString()} edges · edges ${lastBuild.edgesMs.toFixed(0)} ms · build ${lastBuild.setDataMs.toFixed(0)} ms`;
  highlightGraph(currentReference()?.track.id ?? null);
}

function highlightGraph(id: number | null) {
  if (!graph) return;
  const i = id == null ? null : graphIndex.get(id) ?? null;
  const related = suggestions.slice(0, 20).map((s) => graphIndex.get(s.track.id)!).filter((x) => x != null);
  graph.setHighlight(i, i == null ? [] : related);
}

function segment(sel: string, attr: string, onPick: (v: string) => void) {
  document.querySelectorAll<HTMLButtonElement>(`${sel} button`).forEach((b) =>
    b.addEventListener("click", () => {
      document.querySelectorAll(`${sel} button`).forEach((x) => x.classList.toggle("on", x === b));
      onPick(b.dataset[attr]!);
    }),
  );
}
segment("#graph-lib", "lib", (v) => {
  graphLib = v as typeof graphLib;
  makeGraph();
  rebuildGraph();
});
segment("#graph-layout", "layout", (v) => {
  layoutMode = v as typeof layoutMode;
  rebuildGraph();
});
segment("#mode", "mode", (v) => {
  document.body.classList.toggle("export", v === "export");
  window.dispatchEvent(new Event("resize"));
});

// ---- drag from the graph -----------------------------------------------------------------

function startGhostDrag(t: Track | undefined, ev: MouseEvent) {
  if (!t) return;
  const ghost = $("#drag-ghost");
  ghost.textContent = t.title;
  const move = (e: MouseEvent) => {
    ghost.style.display = "block";
    ghost.style.left = `${e.clientX + 12}px`;
    ghost.style.top = `${e.clientY + 8}px`;
    document.querySelectorAll(".deck").forEach((d) => d.classList.toggle("drop", d.contains(document.elementFromPoint(e.clientX, e.clientY))));
  };
  const up = (e: MouseEvent) => {
    window.removeEventListener("mousemove", move, true);
    window.removeEventListener("mouseup", up, true);
    ghost.style.display = "none";
    document.querySelectorAll(".deck").forEach((d) => d.classList.remove("drop"));
    const deckEl = document.elementFromPoint(e.clientX, e.clientY)?.closest<HTMLElement>(".deck");
    if (deckEl && ghost.dataset.moved) loadToDeck(+deckEl.dataset.deck!, t.id);
    else setSelected(t.id, "graph");
    delete ghost.dataset.moved;
  };
  const start = { x: ev.clientX, y: ev.clientY };
  window.addEventListener("mousemove", (e) => {
    if (Math.hypot(e.clientX - start.x, e.clientY - start.y) > 4) ghost.dataset.moved = "1";
  }, { once: true });
  window.addEventListener("mousemove", move, true);
  window.addEventListener("mouseup", up, true);
}

// ---- decks -------------------------------------------------------------------------------

async function loadToDeck(deck: number, id: number) {
  const t = byId.get(id);
  if (!t) return;
  const view = decks[deck];
  try {
    const loaded = await invoke<LoadedTrack>("load_track", { deck, source: t.source });
    await view.setTrack(t, loaded);
  } catch (e) {
    console.error(e);
    alert(`Could not load ${t.title}: ${e}`);
  }
}

function libraryControl(c: UiControl) {
  if (c.key === "MoveVertical") {
    const list = focusedPane === "library" ? libList : sugList;
    list.select((list.selected < 0 ? -1 : list.selected) + Math.round(c.value));
  } else if (c.key === "TogglePane") {
    if (c.value > 0) focusPane(focusedPane === "library" ? "suggest" : "library");
  } else if (c.key === "LoadSelectedTrack" && c.value > 0) {
    const deck = c.group === "[Channel2]" ? 1 : 0;
    const t = focusedPane === "library" ? libList.items[libList.selected] : sugList.items[sugList.selected]?.track;
    if (t) loadToDeck(deck, t.id);
  }
}
listen<UiControl>("ui-control", libraryControl);

// ---- mixer --------------------------------------------------------------------------------

$("#mixer").innerHTML = `
  <label>Volume A<input type="range" min="0" max="1" step="0.01" value="1" data-mix="[Channel1] volume" /></label>
  <label>Volume B<input type="range" min="0" max="1" step="0.01" value="1" data-mix="[Channel2] volume" /></label>
  <label>Crossfader<input type="range" min="-1" max="1" step="0.01" value="0" data-mix="[Master] crossfader" /></label>
  <label style="flex-direction:row;align-items:center;gap:6px"><input type="checkbox" checked data-q /> Quantise</label>
  <span class="muted" style="font-size:11px">Master → out 1/2<br>Headphones → out 3/4</span>`;
document.querySelectorAll<HTMLInputElement>("[data-mix]").forEach((el) => {
  const [g, k] = el.dataset.mix!.split(" ");
  el.addEventListener("input", () => control(g, k, +el.value));
  el.addEventListener("dblclick", () => {
    el.value = k === "crossfader" ? "0" : "1";
    control(g, k, +el.value);
  });
});
$<HTMLInputElement>("[data-q]").addEventListener("change", (e) => control("[Master]", "quantize", (e.target as HTMLInputElement).checked ? 1 : 0));

// ---- keyboard ------------------------------------------------------------------------------

function sendBinding(b: KeyBinding, value: number) {
  if (b.group === "[Library]" || b.key === "LoadSelectedTrack") libraryControl({ group: b.group, key: b.key, value });
  else control(b.group, b.key, value);
}

installKeyboard({
  send: sendBinding,
  isOn: (group, key) => decks[group === "[Channel2]" ? 1 : 0].isOn(key),
  onLayer: (typing) => {
    const el = $("#key-layer");
    el.textContent = typing ? "TYPING" : "DECK KEYS";
    el.classList.toggle("typing", typing);
  },
  focusSearch: () => $<HTMLInputElement>("#search").focus(),
});
$("#help").innerHTML = keymapHelp();
$("#help-btn").addEventListener("click", () => $<HTMLDialogElement>("#help").showModal());

// ---- engine state, audio and MIDI status ---------------------------------------------------

let worstCallback = 0;
listen<EngineState>("engine-state", (s) => {
  const now = performance.now();
  engine = s;
  worstCallback = Math.max(worstCallback, s.audio.max_callback_ms);
  s.decks.forEach((d, i) => {
    const was = decks[i].state?.playing;
    decks[i].update(d, now);
    if (d.playing && !was) {
      lastStarted = i;
      playStarted[i] = now;
    }
    if (!d.playing) playStarted[i] = null;
    const t = decks[i].track;
    if (t && playStarted[i] != null && now - playStarted[i]! > 30_000) played.add(t.id);
  });
});

async function showAudio() {
  const info = await invoke<OutputInfo | null>("audio_info");
  const el = $("#audio-status");
  if (!info) {
    el.textContent = "no audio";
    return;
  }
  el.textContent = `${info.null ? "⚠ " : ""}${info.device} · ${info.sample_rate / 1000} kHz · ${info.buffer_frames ?? "default"} frames · ${info.channels} ch`;
  el.className = `pill ${info.null ? "warn" : "ok"}`;
  el.title = info.null
    ? "No audio device: the engine is running on a timer so the UI can be tested. Set EARSHOT_AUDIO_DEVICE or check your sound card."
    : `${info.sample_format}. Master on outputs 1/2${info.channels >= 4 ? ", headphones on 3/4" : " (no headphone outputs on this device)"}.`;
}

async function connectMidi() {
  const el = $("#midi-status");
  try {
    const ports = await invoke<string[]>("midi_connect");
    el.textContent = ports.length ? `MIDI: ${ports.join(", ")}` : "MIDI: none";
    el.className = `pill ${ports.length ? "ok" : ""}`;
  } catch (e) {
    el.textContent = `MIDI: ${e}`;
  }
}
$("#midi-status").addEventListener("click", connectMidi);

const midiLines: string[] = [];
listen<MidiEvent>("midi", (m) => {
  const hex = m.bytes.map((b) => b.toString(16).padStart(2, "0").toUpperCase()).join(" ");
  midiLines.unshift(`${hex} → ${m.mapped ?? "unmapped"}`);
  midiLines.length = Math.min(midiLines.length, 6);
  $("#midi-log").textContent = `MIDI  ${midiLines.join("   |   ")}`;
});

// ---- frame loop and HUD ---------------------------------------------------------------------

const hud = new FrameStats();
let bench: FrameStats | null = null;
let benchTick: ((now: number) => void) | null = null;
let lastFrame = performance.now();
function frame(now: number) {
  const dt = now - lastFrame;
  lastFrame = now;
  hud.add(dt);
  bench?.add(dt);
  benchTick?.(now);
  for (const d of decks) d.frame(now);
  requestAnimationFrame(frame);
}
requestAnimationFrame(frame);

setInterval(() => {
  hud.trim(2000);
  const s = hud.summary();
  $("#fps").textContent = `${s.avgFps.toFixed(0)} fps · 1% ${s.low1Fps.toFixed(0)} · worst ${s.worstMs.toFixed(0)} ms`;
  if (engine) {
    const a = engine.audio;
    const el = $("#audio-load");
    el.textContent = `cb ${worstCallback.toFixed(2)}/${a.budget_ms.toFixed(1)} ms · slow ${a.slow_callbacks} · xrun ${a.xruns}`;
    el.style.color = a.xruns || worstCallback > a.budget_ms * 0.8 ? "var(--bad)" : "";
    worstCallback = 0;
  }
}, 500);

// ---- benchmark ------------------------------------------------------------------------------

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const nextFrame = () => new Promise((r) => requestAnimationFrame(r));

async function runBenchmark(log: (s: string) => void) {
  const DURATION = 12_000;
  const env = {
    when: new Date().toISOString(),
    userAgent: navigator.userAgent,
    tauri: inTauri,
    window: `${innerWidth}x${innerHeight} @${devicePixelRatio}x`,
    gpu: gpuInfo(),
    audio: await invoke<OutputInfo | null>("audio_info"),
  };
  log(`${env.window} · ${env.gpu}\n${env.userAgent}\n`);
  document.body.classList.remove("export");
  window.dispatchEvent(new Event("resize"));

  // Two demo tracks: a drifting "live" one on A and a steady one on B, synced.
  const demoA: Track = { ...syntheticLibrary(1, 900_001)[0], title: "Bench A (124→127 live)", source: { kind: "demo", bpm: 124, end_bpm: 127, key: "8A", seed: 11, title: "Bench A (124→127 live)" } };
  const demoB: Track = { ...syntheticLibrary(1, 900_002, 9)[0], title: "Bench B (125)", source: { kind: "demo", bpm: 125, key: "9A", seed: 12, title: "Bench B (125)" } };
  for (const [d, t] of [[0, demoA], [1, demoB]] as const) {
    byId.set(t.id, t);
    await loadToDeck(d, t.id);
  }
  control("[Channel1]", "play", 1);
  control("[Channel2]", "sync_enabled", 1);
  control("[Channel2]", "play", 1);

  const results: Record<string, unknown>[] = [];
  const keepLib = graphLib;
  const keepN = synthCount;
  for (const lib of ["sigma", "cosmos"] as const) {
    for (const n of [2000, 10000]) {
      graphLib = lib;
      synthCount = Math.max(0, n - tracks.length);
      makeGraph();
      rebuildGraph();
      await sleep(1000);
      const startAudio = engine ? { ...engine.audio } : null;
      let worstCb = 0;
      bench = new FrameStats();
      const t0 = performance.now();
      let lastSel = 0;
      await new Promise<void>((done) => {
        benchTick = (now) => {
          const t = (now - t0) / DURATION;
          graph?.benchFrame(Math.min(1, t));
          if (engine) worstCb = Math.max(worstCb, engine.audio.max_callback_ms);
          // Change the selection every 2 s, like browsing while playing.
          if (now - lastSel > 2000) {
            lastSel = now;
            highlightGraph(graphNodes[Math.floor(Math.random() * graphNodes.length)].id);
          }
          if (t >= 1) {
            benchTick = null;
            done();
          }
        };
      });
      const s = bench.summary();
      bench = null;
      const r = {
        graph: lib,
        nodes: graphNodes.length,
        edges: lastBuild.edges,
        edgesMs: Math.round(lastBuild.edgesMs),
        buildMs: Math.round(lastBuild.setDataMs),
        ...Object.fromEntries(Object.entries(s).map(([k, v]) => [k, Math.round(v * 10) / 10])),
        audioSlowCallbacks: engine && startAudio ? engine.audio.slow_callbacks - startAudio.slow_callbacks : null,
        audioXruns: engine && startAudio ? engine.audio.xruns - startAudio.xruns : null,
        audioWorstCallbackMs: Math.round(worstCb * 100) / 100,
        audioBudgetMs: engine?.audio.budget_ms ?? null,
      };
      results.push(r);
      log(
        `${lib.padEnd(6)} ${String(r.nodes).padStart(6)} nodes  ${s.avgFps.toFixed(1).padStart(5)} fps avg  ${s.low1Fps.toFixed(1).padStart(5)} 1%-low  worst ${s.worstMs.toFixed(0).padStart(4)} ms  >33ms ${String(s.over33ms).padStart(3)}  | audio worst ${r.audioWorstCallbackMs} ms, slow ${r.audioSlowCallbacks}, xruns ${r.audioXruns}`,
      );
      await nextFrame();
    }
  }
  control("[Channel1]", "play", 0);
  control("[Channel2]", "play", 0);
  graphLib = keepLib;
  synthCount = keepN;
  makeGraph();
  rebuildGraph();
  graph?.resetCamera();
  const name = `bench-${env.when.replace(/[:.]/g, "-")}`;
  const path = await invoke<string>("save_report", { name, json: JSON.stringify({ env, results }, null, 2) });
  log(`\nSaved ${path}`);
}

$("#bench-btn").addEventListener("click", () => $<HTMLDialogElement>("#bench-dialog").showModal());
$("#bench-close").addEventListener("click", () => $<HTMLDialogElement>("#bench-dialog").close());
$("#bench-run").addEventListener("click", async () => {
  const out = $("#bench-out");
  const btn = $<HTMLButtonElement>("#bench-run");
  out.textContent = "";
  btn.disabled = true;
  try {
    await runBenchmark((s) => (out.textContent += `${s}\n`));
  } catch (e) {
    out.textContent += `\nFailed: ${e}`;
  } finally {
    btn.disabled = false;
  }
});

// ---- start --------------------------------------------------------------------------------

void refreshFolders().then(loadLibrary);
void showAudio();
void connectMidi();
