// Graph view: one node per track, edges between compatible tracks. Two renderers
// behind one interface so the spike can compare them: Sigma.js and cosmos.gl.

import { Graph as CosmosGraph } from "@cosmos.gl/graph";
import Graph from "graphology";
import Sigma from "sigma";
import { parseKey } from "./library";
import type { Track } from "./types";

export interface GraphView {
  readonly name: string;
  setData(tracks: Track[], pos: Float32Array, edges: [number, number][]): void;
  /** Indices into the track array passed to setData. */
  setHighlight(selected: number | null, related: number[]): void;
  /** Drive the camera along a fixed path for the benchmark; t runs 0..1. */
  benchFrame(t: number): void;
  resetCamera(): void;
  destroy(): void;
}

export interface GraphCallbacks {
  onSelect(index: number | null): void;
  onDragStart(index: number, ev: MouseEvent): void;
}

// ---- layout and colour ---------------------------------------------------------------

/** Positions in -1..1, interleaved x,y. */
export function computeLayout(tracks: Track[], mode: "wheel" | "vibe"): Float32Array {
  const pos = new Float32Array(tracks.length * 2);
  const vibePos = mode === "vibe" ? pca2(tracks) : null;
  tracks.forEach((t, i) => {
    const jx = hash(t.id * 2 + 1) - 0.5;
    const jy = hash(t.id * 2 + 2) - 0.5;
    if (vibePos && t.vibe) {
      pos[2 * i] = vibePos[2 * i] + jx * 0.04;
      pos[2 * i + 1] = vibePos[2 * i + 1] + jy * 0.04;
      return;
    }
    // Key wheel: angle = Camelot position (relative keys side by side), radius = BPM.
    const k = parseKey(t.key);
    const angle = k ? ((k.n - 1 + (k.minor ? 0 : 0.5) + jx * 0.4) / 12) * 2 * Math.PI : hash(t.id) * 2 * Math.PI;
    const bpm = t.bpm ?? 60;
    const r = 0.15 + 0.85 * Math.min(1, Math.max(0, (bpm - 90) / 90)) + jy * 0.03;
    pos[2 * i] = r * Math.sin(angle);
    pos[2 * i + 1] = r * Math.cos(angle);
  });
  return pos;
}

function hash(n: number) {
  let x = Math.imul(n ^ 0x9e3779b9, 0x85ebca6b);
  x ^= x >>> 13;
  x = Math.imul(x, 0xc2b2ae35);
  x ^= x >>> 16;
  return (x >>> 0) / 4294967296;
}

/** First two principal components of the vibe vectors, scaled to about -1..1. */
function pca2(tracks: Track[]): Float32Array {
  const vs = tracks.map((t) => t.vibe).filter((v): v is Float32Array => !!v);
  const out = new Float32Array(tracks.length * 2);
  if (vs.length < 3) return out;
  const d = vs[0].length;
  const mean = new Float64Array(d);
  for (const v of vs) for (let k = 0; k < d; k++) mean[k] += v[k] / vs.length;
  const comps: Float64Array[] = [];
  for (let c = 0; c < 2; c++) {
    let w: Float64Array = new Float64Array(d).map((_, k) => Math.sin(k * 7.1 + c));
    for (let it = 0; it < 30; it++) {
      const next = new Float64Array(d);
      for (const v of vs) {
        let p = 0;
        for (let k = 0; k < d; k++) p += (v[k] - mean[k]) * w[k];
        for (let k = 0; k < d; k++) next[k] += p * (v[k] - mean[k]);
      }
      for (const prev of comps) {
        let p = 0;
        for (let k = 0; k < d; k++) p += next[k] * prev[k];
        for (let k = 0; k < d; k++) next[k] -= p * prev[k];
      }
      const n = Math.hypot(...next) || 1;
      w = next.map((x) => x / n);
    }
    comps.push(w);
  }
  let max = 1e-9;
  tracks.forEach((t, i) => {
    if (!t.vibe) return;
    for (let c = 0; c < 2; c++) {
      let p = 0;
      for (let k = 0; k < d; k++) p += (t.vibe[k] - mean[k]) * comps[c][k];
      out[2 * i + c] = p;
      max = Math.max(max, Math.abs(p));
    }
  });
  for (let i = 0; i < out.length; i++) out[i] /= max;
  return out;
}

/** Colour by Camelot key, like the key wheel DJs know. Minor keys slightly darker. */
export function keyColor(key: string | null): [number, number, number] {
  const k = parseKey(key);
  if (!k) return [0.55, 0.55, 0.6];
  const h = ((k.n - 1) / 12) * 360;
  return hsl(h, 0.75, k.minor ? 0.55 : 0.68);
}

function hsl(h: number, s: number, l: number): [number, number, number] {
  const f = (n: number) => {
    const k = (n + h / 30) % 12;
    return l - s * Math.min(l, 1 - l) * Math.max(-1, Math.min(k - 3, 9 - k, 1));
  };
  return [f(0), f(8), f(4)];
}

const css = ([r, g, b]: number[]) => `rgb(${Math.round(r * 255)},${Math.round(g * 255)},${Math.round(b * 255)})`;
const nodeSize = (t: Track) => 2 + 4 * t.energy;

/** Camera path shared by both renderers: a circle round the wheel while zooming in and out. */
function benchCamera(t: number) {
  const a = 2 * Math.PI * t;
  return { cx: 0.45 * Math.cos(a), cy: 0.45 * Math.sin(a), zoom: 1.2 + 2.8 * (0.5 - 0.5 * Math.cos(4 * Math.PI * t)) };
}

// ---- Sigma.js -------------------------------------------------------------------------

export class SigmaView implements GraphView {
  readonly name = "sigma";
  private sigma: Sigma | null = null;
  private graph = new Graph();
  private selected: string | null = null;
  private related = new Set<string>();
  private el: HTMLElement;

  constructor(container: HTMLElement, private cb: GraphCallbacks) {
    this.el = document.createElement("div");
    container.appendChild(this.el);
  }

  setData(tracks: Track[], pos: Float32Array, edges: [number, number][]) {
    this.sigma?.kill();
    this.graph = new Graph();
    tracks.forEach((t, i) => {
      this.graph.addNode(String(i), {
        x: pos[2 * i],
        y: pos[2 * i + 1],
        size: nodeSize(t),
        color: css(keyColor(t.key)),
        label: `${t.title} · ${t.bpm?.toFixed(0) ?? "?"} ${t.key ?? ""}`,
      });
    });
    for (const [a, b] of edges) this.graph.addEdge(String(a), String(b), { size: 0.4, color: "#2c303d" });
    this.sigma = new Sigma(this.graph, this.el, {
      labelRenderedSizeThreshold: 9,
      allowInvalidContainer: true, // hidden while the flow is full screen
      hideEdgesOnMove: false,
      zIndex: true,
      nodeReducer: (node, data) => {
        if (this.selected === null) return data;
        if (node === this.selected) return { ...data, size: data.size * 2.4, highlighted: true, zIndex: 2 };
        if (this.related.has(node)) return { ...data, size: data.size * 1.7, zIndex: 1 };
        return { ...data, color: "#3a3c46", label: null, zIndex: 0 };
      },
    });
    this.sigma.on("clickNode", ({ node }) => this.cb.onSelect(+node));
    this.sigma.on("clickStage", () => this.cb.onSelect(null));
    this.sigma.on("downNode", (p) => {
      p.preventSigmaDefault();
      if (p.event.original instanceof MouseEvent) this.cb.onDragStart(+p.node, p.event.original);
    });
  }

  setHighlight(selected: number | null, related: number[]) {
    this.selected = selected === null ? null : String(selected);
    this.related = new Set(related.map(String));
    this.sigma?.refresh();
  }

  benchFrame(t: number) {
    const c = benchCamera(t);
    // Sigma's camera works in normalised 0..1 coordinates of the graph's bounding box.
    this.sigma?.getCamera().setState({ x: 0.5 + c.cx / 2, y: 0.5 + c.cy / 2, ratio: 1 / c.zoom });
  }

  resetCamera() {
    this.sigma?.getCamera().setState({ x: 0.5, y: 0.5, ratio: 1, angle: 0 });
  }

  destroy() {
    this.sigma?.kill();
    this.el.remove();
  }
}

// ---- cosmos.gl ------------------------------------------------------------------------

const SPACE = 4096;

export class CosmosView implements GraphView {
  readonly name = "cosmos";
  private graph: CosmosGraph;
  private el: HTMLDivElement;
  private hovered: number | undefined;
  private colors = new Float32Array();
  private sizes = new Float32Array();

  constructor(container: HTMLElement, private cb: GraphCallbacks) {
    this.el = document.createElement("div");
    container.appendChild(this.el);
    this.graph = new CosmosGraph(this.el, {
      spaceSize: SPACE,
      enableSimulation: false,
      backgroundColor: "#17181d",
      linkDefaultColor: [0.17, 0.19, 0.24, 1],
      renderLinks: true,
      scalePointsOnZoom: false,
      rescalePositions: false,
      fitViewOnInit: true,
      fitViewPadding: 0.1,
      hoveredPointCursor: "grab",
      attribution: "",
      onClick: (index) => this.cb.onSelect(index ?? null),
      onPointMouseOver: (index) => (this.hovered = index),
      onPointMouseOut: () => (this.hovered = undefined),
    });
    // Start a drag on a node before d3-zoom sees the mousedown and starts panning.
    this.el.addEventListener(
      "mousedown",
      (ev) => {
        if (this.hovered !== undefined) {
          ev.stopPropagation();
          this.cb.onDragStart(this.hovered, ev);
        }
      },
      true,
    );
  }

  setData(tracks: Track[], pos: Float32Array, edges: [number, number][]) {
    const p = new Float32Array(pos.length);
    for (let i = 0; i < pos.length; i++) p[i] = SPACE / 2 + pos[i] * (SPACE * 0.45);
    this.colors = new Float32Array(tracks.length * 4);
    this.sizes = new Float32Array(tracks.length);
    tracks.forEach((t, i) => {
      const [r, g, b] = keyColor(t.key);
      this.colors.set([r, g, b, 1], i * 4);
      this.sizes[i] = nodeSize(t) * 1.5;
    });
    const links = new Float32Array(edges.length * 2);
    edges.forEach(([a, b], i) => {
      links[2 * i] = a;
      links[2 * i + 1] = b;
    });
    this.graph.setPointPositions(p);
    this.graph.setPointColors(this.colors);
    this.graph.setPointSizes(this.sizes);
    this.graph.setLinks(links);
    this.graph.render();
    this.graph.fitView(0, 0.1);
  }

  setHighlight(selected: number | null, related: number[]) {
    const colors = this.colors.slice();
    const sizes = this.sizes.slice();
    if (selected !== null) {
      const keep = new Set([selected, ...related]);
      for (let i = 0; i < sizes.length; i++) {
        if (!keep.has(i)) colors.set([0.23, 0.24, 0.28, 1], i * 4);
      }
      for (const i of related) sizes[i] *= 1.7;
      sizes[selected] *= 2.4;
    }
    this.graph.setPointColors(colors);
    this.graph.setPointSizes(sizes);
    this.graph.render();
  }

  benchFrame(t: number) {
    const c = benchCamera(t);
    const cx = SPACE / 2 + c.cx * SPACE * 0.45;
    const cy = SPACE / 2 + c.cy * SPACE * 0.45;
    const r = (SPACE * 0.5) / c.zoom;
    this.graph.fitViewByPointPositions([cx - r, cy - r, cx + r, cy + r], 0, 0);
  }

  resetCamera() {
    this.graph.fitView(0, 0.1);
  }

  destroy() {
    this.graph.destroy();
    this.el.remove();
  }
}
