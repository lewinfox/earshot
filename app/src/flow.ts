// Set-building canvas: add a track or two, then grow the set from suggestions. Each
// card has a "+" on its right that opens nine suggested next tracks (with Drop / Hold /
// Build); picking one adds it as a new card joined to the first. Cards can be dragged,
// the canvas panned (drag the background) and zoomed (wheel). The layout is kept in
// this browser's local storage.

import { parseKey, search } from "./library";
import { effectiveKey, suggest, type Scored } from "./suggest";
import type { Track } from "./types";

interface FlowNode {
  id: string;
  trackId: number;
  parent: string | null;
  x: number;
  y: number;
}

export interface FlowDeps {
  tracks: () => Track[];
  byId: (id: number) => Track | undefined;
  /** Load a track onto a deck that isn't playing. */
  load: (trackId: number) => void;
}

const NODE_W = 236;
const NODE_H = 92;
const GAP_X = 90;
const GAP_Y = 22;
const STORE = "earshot.flow";
const ENERGY = [
  { label: "Drop", delta: -0.15 },
  { label: "Hold", delta: 0 },
  { label: "Build", delta: 0.15 },
];

const esc = (s: string) => s.replace(/[&<>"]/g, (c) => `&#${c.charCodeAt(0)};`);
const mmss = (s: number) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, "0")}`;

/** Camelot key colour, round the wheel like the usual key charts; B (major) lighter. */
export function keyColour(key: string | null): string {
  const k = parseKey(key);
  if (!k) return "#555";
  return `hsl(${((k.n - 1) * 30 + 150) % 360} 70% ${k.minor ? 58 : 72}%)`;
}

/** Energy as 1..10 for display. */
const energy10 = (t: Track) => Math.max(1, Math.min(10, Math.round(t.energy * 9 + 1)));

function chips(t: Track) {
  return `<span class="chip">${t.bpm != null ? (Math.round(t.bpm * 10) / 10).toString() : "–"}</span>
    <span class="chip key" style="--c:${keyColour(t.key)}">${t.key ?? "?"}</span>
    <span class="chip energy" title="Energy (stand-in: loudness and brightness, ranked across your library)">${energy10(t)}</span>`;
}

/** A coloured tile with the artist's initials, standing in for artwork. */
function art(t: Track) {
  let h = 0;
  for (const c of t.artist + t.title) h = (h * 31 + c.charCodeAt(0)) % 360;
  const initials = (t.artist || t.title)
    .split(/\s+/)
    .slice(0, 2)
    .map((w) => w[0] ?? "")
    .join("")
    .toUpperCase();
  return `<span class="fn-art" style="--h:${h}">${esc(initials)}</span>`;
}

export class FlowView {
  private nodes = new Map<string, FlowNode>();
  private view = { x: 40, y: 40, k: 1 };
  private viewport: HTMLElement;
  private world: HTMLElement;
  private edges: SVGSVGElement;
  private pop: HTMLElement | null = null;
  private energyDelta = 0;
  private seq = 0;

  constructor(
    private el: HTMLElement,
    private deps: FlowDeps,
  ) {
    el.innerHTML = `
      <div class="flow-bar">
        <span class="flow-summary"></span>
        <span class="spacer"></span>
        <button data-act="add">Add track</button>
        <button data-act="full" title="Hide the decks while planning">Full screen</button>
        <button data-act="fit" title="Fit everything in view">Fit</button>
        <button data-act="reset" title="Remove every card">Reset</button>
      </div>
      <div class="flow-viewport">
        <div class="flow-world">
          <svg class="flow-edges"></svg>
        </div>
        <div class="flow-empty">Add a track to start: <b>Add track</b> above, or right-click a track in the list → <b>Add to flow</b>.</div>
      </div>`;
    this.viewport = el.querySelector(".flow-viewport")!;
    this.world = el.querySelector(".flow-world")!;
    this.edges = el.querySelector(".flow-edges")!;
    el.querySelector('[data-act="add"]')!.addEventListener("click", (e) => this.openSearch(e.currentTarget as HTMLElement));
    el.querySelector('[data-act="full"]')!.addEventListener("click", (e) => {
      const on = document.body.classList.toggle("flow-full");
      (e.currentTarget as HTMLElement).classList.toggle("on", on);
      window.dispatchEvent(new Event("resize"));
    });
    el.querySelector('[data-act="fit"]')!.addEventListener("click", () => this.fit());
    el.querySelector('[data-act="reset"]')!.addEventListener("click", () => {
      if (this.nodes.size && confirm("Remove every card from the flow?")) {
        this.nodes.clear();
        this.render();
      }
    });
    this.installPanZoom();
    this.restore();
  }

  /** Add a track as a new starting card, below the existing ones. */
  addRoot(trackId: number) {
    const bottom = Math.max(-GAP_Y - NODE_H, ...[...this.nodes.values()].map((n) => n.y));
    const left = Math.min(0, ...[...this.nodes.values()].map((n) => n.x));
    const n = this.add(trackId, null, left, bottom + NODE_H + GAP_Y * 3);
    this.render();
    this.reveal(n);
  }

  /** Track data changed (library rescanned, analysis finished): redraw the cards. */
  refresh() {
    for (const [id, n] of this.nodes) if (!this.deps.byId(n.trackId)) this.nodes.delete(id);
    this.render();
  }

  // ---- model -----------------------------------------------------------------------

  private add(trackId: number, parent: string | null, x: number, y: number): FlowNode {
    const n = { id: `n${Date.now().toString(36)}${this.seq++}`, trackId, parent, x, y };
    this.nodes.set(n.id, n);
    return n;
  }

  private addChild(parent: FlowNode, trackId: number) {
    const kids = [...this.nodes.values()].filter((n) => n.parent === parent.id);
    const y = kids.length ? Math.max(...kids.map((k) => k.y)) + NODE_H + GAP_Y : parent.y;
    const n = this.add(trackId, parent.id, parent.x + NODE_W + GAP_X, y);
    this.render();
    this.reveal(n);
  }

  /** Remove a card; its children join its parent. */
  private remove(id: string) {
    const gone = this.nodes.get(id);
    if (!gone) return;
    for (const n of this.nodes.values()) if (n.parent === id) n.parent = gone.parent;
    this.nodes.delete(id);
    this.render();
  }

  private inFlow() {
    return new Set([...this.nodes.values()].map((n) => n.trackId));
  }

  private save() {
    try {
      localStorage.setItem(STORE, JSON.stringify({ nodes: [...this.nodes.values()], view: this.view }));
    } catch {
      /* storage unavailable: the flow just isn't kept */
    }
  }

  private restore() {
    try {
      const s = JSON.parse(localStorage.getItem(STORE) ?? "null");
      if (s?.nodes) for (const n of s.nodes as FlowNode[]) this.nodes.set(n.id, n);
      if (s?.view) this.view = s.view;
    } catch {
      /* nothing saved */
    }
    this.render();
  }

  // ---- drawing -----------------------------------------------------------------------

  private render() {
    this.closePop();
    this.world.querySelectorAll(".fnode").forEach((e) => e.remove());
    for (const n of this.nodes.values()) {
      const t = this.deps.byId(n.trackId);
      if (t) this.world.append(this.card(n, t));
    }
    this.drawEdges();
    this.applyView();
    this.summarise();
    (this.el.querySelector(".flow-empty") as HTMLElement).hidden = this.nodes.size > 0;
    this.save();
  }

  private card(n: FlowNode, t: Track): HTMLElement {
    const c = document.createElement("div");
    c.className = "fnode";
    c.dataset.id = n.id;
    c.style.left = `${n.x}px`;
    c.style.top = `${n.y}px`;
    c.title = t.path ?? "";
    c.innerHTML = `
      <i class="fn-port in"></i>
      <div class="fn-head">${art(t)}<div class="fn-id"><b>${esc(t.title)}</b><small>${esc(t.artist)}</small></div>
        <button class="fn-play" title="Load to a deck that isn't playing">▶</button></div>
      <div class="fn-chips">${chips(t)}<span class="spacer"></span><span class="fn-time">${t.duration ? mmss(t.duration) : ""}</span>
        <button class="fn-del" title="Remove from the flow">✕</button></div>
      <button class="fn-plus" title="Suggest what to play next">+</button>`;
    c.querySelector(".fn-play")!.addEventListener("click", () => this.deps.load(t.id));
    c.querySelector(".fn-del")!.addEventListener("click", () => this.remove(n.id));
    c.querySelector(".fn-plus")!.addEventListener("click", () => this.openSuggestions(n));
    c.addEventListener("dblclick", () => this.deps.load(t.id));
    this.dragCard(c, n);
    return c;
  }

  private drawEdges() {
    const paths: string[] = [];
    for (const n of this.nodes.values()) {
      const p = n.parent ? this.nodes.get(n.parent) : null;
      if (!p) continue;
      const [x1, y1, x2, y2] = [p.x + NODE_W, p.y + NODE_H / 2, n.x, n.y + NODE_H / 2];
      const dx = Math.max(40, Math.abs(x2 - x1) / 2);
      paths.push(`<path d="M${x1},${y1} C${x1 + dx},${y1} ${x2 - dx},${y2} ${x2},${y2}"/>`);
    }
    this.edges.innerHTML = paths.join("");
  }

  private summarise() {
    const ts = [...this.nodes.values()].map((n) => this.deps.byId(n.trackId)).filter((t): t is Track => !!t);
    const bpms = ts.map((t) => t.bpm).filter((b): b is number => b != null);
    const secs = ts.reduce((s, t) => s + (t.duration ?? 0), 0);
    (this.el.querySelector(".flow-summary") as HTMLElement).textContent = ts.length
      ? `${ts.length} track${ts.length > 1 ? "s" : ""} · ${mmss(secs)}${bpms.length ? ` · ${Math.round(Math.min(...bpms))}–${Math.round(Math.max(...bpms))} BPM` : ""}`
      : "";
  }

  // ---- suggestions -------------------------------------------------------------------

  private openSuggestions(n: FlowNode) {
    const t = this.deps.byId(n.trackId);
    if (!t) return;
    this.closePop();
    const pop = document.createElement("div");
    pop.className = "flow-pop suggest-pop";
    const draw = () => {
      const exclude = this.inFlow();
      const picks: Scored[] = suggest(
        { track: t, bpm: t.bpm, key: effectiveKey(t.key, 1), energyDelta: this.energyDelta, played: exclude },
        this.deps.tracks(),
        40,
      )
        .filter((s) => !exclude.has(s.track.id))
        .slice(0, 9);
      pop.innerHTML = `
        <div class="pop-head">
          <div class="seg">${ENERGY.map((e) => `<button data-e="${e.delta}" class="${e.delta === this.energyDelta ? "on" : ""}">${e.label}</button>`).join("")}</div>
          <span class="spacer"></span><button class="pop-close">Close ✕</button>
        </div>
        <div class="sugg-grid">${
          picks.length
            ? picks
                .map(
                  (s) => `<div class="sugg" data-id="${s.track.id}" title="${esc(s.track.path ?? "")}">
                    ${art(s.track)}<div class="fn-id"><b>${esc(s.track.title)}</b><small>${esc(s.track.artist)}</small></div>
                    <div class="fn-chips">${chips(s.track)}<span class="spacer"></span><span class="score" title="Match">${Math.round(s.total * 100)}</span></div>
                  </div>`,
                )
                .join("")
            : `<div class="muted">No suggestions: the library needs more tracks with BPM and key.</div>`
        }</div>`;
      pop.querySelectorAll<HTMLButtonElement>("[data-e]").forEach((b) =>
        b.addEventListener("click", () => {
          this.energyDelta = +b.dataset.e!;
          draw();
        }),
      );
      pop.querySelector(".pop-close")!.addEventListener("click", () => this.closePop());
      pop.querySelectorAll<HTMLElement>(".sugg").forEach((c) => c.addEventListener("click", () => this.addChild(n, +c.dataset.id!)));
    };
    draw();
    // In screen space, beside the card's "+", kept inside the visible canvas.
    this.viewport.append(pop);
    this.pop = pop;
    const vw = this.viewport.clientWidth;
    const vh = this.viewport.clientHeight;
    const { x, y, k } = this.view;
    const px = (n.x + NODE_W) * k + x + 24;
    const py = (n.y + NODE_H / 2) * k + y - pop.offsetHeight / 2;
    pop.style.left = `${Math.max(8, Math.min(px, vw - pop.offsetWidth - 8))}px`;
    pop.style.top = `${Math.max(8, Math.min(py, vh - pop.offsetHeight - 8))}px`;
  }

  private openSearch(anchor: HTMLElement) {
    this.closePop();
    const pop = document.createElement("div");
    pop.className = "flow-pop search";
    const r = anchor.getBoundingClientRect();
    const host = this.el.getBoundingClientRect();
    pop.style.right = `${host.right - r.right}px`;
    pop.style.top = `${r.bottom - host.top + 4}px`;
    pop.innerHTML = `<input type="search" placeholder="Search the library   bpm:120-126  key:8A" /><div class="search-list"></div>`;
    const input = pop.querySelector("input")!;
    const list = pop.querySelector(".search-list")!;
    const draw = () => {
      const hits = search(this.deps.tracks(), input.value).slice(0, 50);
      list.innerHTML = hits
        .map((t) => `<div class="search-item" data-id="${t.id}"><span class="t">${esc(t.title)} <small>${esc(t.artist)}</small></span>${chips(t)}</div>`)
        .join("");
      list.querySelectorAll<HTMLElement>(".search-item").forEach((i) =>
        i.addEventListener("click", () => {
          this.addRoot(+i.dataset.id!);
          this.closePop();
        }),
      );
    };
    input.addEventListener("input", draw);
    input.addEventListener("keydown", (e) => e.key === "Escape" && this.closePop());
    draw();
    this.el.append(pop);
    this.pop = pop;
    input.focus();
  }

  private closePop() {
    this.pop?.remove();
    this.pop = null;
  }

  // ---- view: pan, zoom, drag ------------------------------------------------------------

  private applyView() {
    const { x, y, k } = this.view;
    this.world.style.transform = `translate(${x}px, ${y}px) scale(${k})`;
  }

  /** Pan so a box (world coordinates) is visible. */
  private reveal(b: { x: number; y: number; w?: number; h?: number }) {
    const vw = this.viewport.clientWidth;
    const vh = this.viewport.clientHeight;
    const { k } = this.view;
    const [w, h] = [(b.w ?? NODE_W) * k, (b.h ?? NODE_H) * k];
    let sx = b.x * k + this.view.x;
    let sy = b.y * k + this.view.y;
    if (sx + w > vw - 20) this.view.x -= sx + w - (vw - 20);
    if (sy + h > vh - 20) this.view.y -= sy + h - (vh - 20);
    sx = b.x * k + this.view.x;
    sy = b.y * k + this.view.y;
    if (sx < 20) this.view.x += 20 - sx;
    if (sy < 20) this.view.y += 20 - sy;
    this.applyView();
    this.save();
  }

  fit() {
    const ns = [...this.nodes.values()];
    if (!ns.length) return;
    const x0 = Math.min(...ns.map((n) => n.x));
    const y0 = Math.min(...ns.map((n) => n.y));
    const x1 = Math.max(...ns.map((n) => n.x + NODE_W));
    const y1 = Math.max(...ns.map((n) => n.y + NODE_H));
    const vw = this.viewport.clientWidth;
    const vh = this.viewport.clientHeight;
    const k = Math.min(1.2, Math.max(0.3, Math.min((vw - 60) / (x1 - x0), (vh - 60) / (y1 - y0))));
    this.view = { k, x: (vw - (x1 - x0) * k) / 2 - x0 * k, y: (vh - (y1 - y0) * k) / 2 - y0 * k };
    this.applyView();
    this.save();
  }

  private installPanZoom() {
    this.viewport.addEventListener("pointerdown", (e) => {
      if ((e.target as HTMLElement).closest(".fnode, .flow-pop")) return;
      this.closePop();
      const start = { x: e.clientX, y: e.clientY, vx: this.view.x, vy: this.view.y };
      this.viewport.setPointerCapture(e.pointerId);
      this.viewport.classList.add("panning");
      const move = (m: PointerEvent) => {
        this.view.x = start.vx + m.clientX - start.x;
        this.view.y = start.vy + m.clientY - start.y;
        this.applyView();
      };
      const up = () => {
        this.viewport.removeEventListener("pointermove", move);
        this.viewport.classList.remove("panning");
        this.save();
      };
      this.viewport.addEventListener("pointermove", move);
      this.viewport.addEventListener("pointerup", up, { once: true });
    });
    this.viewport.addEventListener(
      "wheel",
      (e) => {
        e.preventDefault();
        const r = this.viewport.getBoundingClientRect();
        const [mx, my] = [e.clientX - r.left, e.clientY - r.top];
        this.closePop();
        const k = Math.min(2, Math.max(0.25, this.view.k * Math.exp(-e.deltaY * 0.0015)));
        // Keep the point under the cursor still.
        this.view.x = mx - ((mx - this.view.x) * k) / this.view.k;
        this.view.y = my - ((my - this.view.y) * k) / this.view.k;
        this.view.k = k;
        this.applyView();
        this.save();
      },
      { passive: false },
    );
  }

  private dragCard(c: HTMLElement, n: FlowNode) {
    c.querySelector(".fn-head")!.addEventListener("pointerdown", (ev) => {
      const e = ev as PointerEvent;
      if ((e.target as HTMLElement).closest("button")) return;
      e.stopPropagation();
      this.closePop();
      const start = { x: e.clientX, y: e.clientY, nx: n.x, ny: n.y };
      c.setPointerCapture(e.pointerId);
      c.classList.add("dragging");
      const move = (m: PointerEvent) => {
        n.x = start.nx + (m.clientX - start.x) / this.view.k;
        n.y = start.ny + (m.clientY - start.y) / this.view.k;
        c.style.left = `${n.x}px`;
        c.style.top = `${n.y}px`;
        this.drawEdges();
      };
      const up = () => {
        c.removeEventListener("pointermove", move);
        c.classList.remove("dragging");
        this.save();
      };
      c.addEventListener("pointermove", move);
      c.addEventListener("pointerup", up, { once: true });
    });
  }
}
