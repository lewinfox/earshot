// Window layout the user can change: drag-to-resize panes, and the UI size (zoom).
// Both are remembered in this browser's local storage.

import { inTauri } from "./api";

const STORE = "earshot.layout";

interface Layout {
  /** CSS pixel sizes, keyed by CSS variable. */
  sizes: Record<string, number>;
  zoom: number;
}

const DEFAULTS: Layout = {
  sizes: { "--tree-w": 250, "--side-w": 380, "--sugg-h": 0, "--wave-h": 72 },
  zoom: 1,
};

export const ZOOMS = [0.8, 0.9, 1, 1.1, 1.25];

/** What each splitter handle changes: its CSS variable, which way, and the limits. */
const HANDLES: Record<string, { v: string; axis: "x" | "y"; sign: 1 | -1; min: number; max: number; scale?: number }> = {
  tree: { v: "--tree-w", axis: "x", sign: 1, min: 140, max: 600 },
  side: { v: "--side-w", axis: "x", sign: -1, min: 240, max: 900 },
  sugg: { v: "--sugg-h", axis: "y", sign: 1, min: 80, max: 2000 },
  // Two waveforms share the drag, so each grows by half of it.
  wave: { v: "--wave-h", axis: "y", sign: 1, min: 30, max: 260, scale: 0.5 },
};

let layout: Layout = load();

function load(): Layout {
  try {
    const s = JSON.parse(localStorage.getItem(STORE) ?? "null");
    if (s) return { sizes: { ...DEFAULTS.sizes, ...s.sizes }, zoom: s.zoom ?? 1 };
  } catch {
    /* nothing saved */
  }
  return structuredClone(DEFAULTS);
}

function save() {
  try {
    localStorage.setItem(STORE, JSON.stringify(layout));
  } catch {
    /* storage unavailable */
  }
}

function applySizes() {
  for (const [k, v] of Object.entries(layout.sizes)) {
    // --sugg-h of 0 means "half the column" (the default split).
    document.documentElement.style.setProperty(k, k === "--sugg-h" && v === 0 ? "1fr" : `${v}px`);
  }
}

let resizeQueued = false;
function relayout() {
  if (resizeQueued) return;
  resizeQueued = true;
  requestAnimationFrame(() => {
    resizeQueued = false;
    window.dispatchEvent(new Event("resize")); // graph and lists re-measure
  });
}

export function installSplitters() {
  applySizes();
  document.querySelectorAll<HTMLElement>("[data-split]").forEach((h) => {
    const cfg = HANDLES[h.dataset.split!];
    h.addEventListener("pointerdown", (e) => {
      e.preventDefault();
      h.setPointerCapture(e.pointerId);
      h.classList.add("active");
      // Start from the pane's current size, so the "1fr" default works too.
      const target = h.dataset.split === "sugg" ? document.querySelector<HTMLElement>("#suggest")! : null;
      const start = layout.sizes[cfg.v] || target?.getBoundingClientRect().height || 0;
      const p0 = cfg.axis === "x" ? e.clientX : e.clientY;
      const z = currentCssZoom();
      const move = (m: PointerEvent) => {
        const d = ((cfg.axis === "x" ? m.clientX : m.clientY) - p0) / z;
        layout.sizes[cfg.v] = Math.round(Math.min(cfg.max, Math.max(cfg.min, start + cfg.sign * d * (cfg.scale ?? 1))));
        applySizes();
        relayout();
      };
      const up = () => {
        h.removeEventListener("pointermove", move);
        h.classList.remove("active");
        save();
      };
      h.addEventListener("pointermove", move);
      h.addEventListener("pointerup", up, { once: true });
    });
    // Double-click: back to the default size.
    h.addEventListener("dblclick", () => {
      layout.sizes[cfg.v] = DEFAULTS.sizes[cfg.v];
      applySizes();
      relayout();
      save();
    });
  });
}

// ---- UI size ---------------------------------------------------------------------------

/** CSS `zoom` is only used outside Tauri (browser mock); pointer maths must divide by it. */
function currentCssZoom() {
  return inTauri ? 1 : layout.zoom;
}

export function zoom() {
  return layout.zoom;
}

export async function setZoom(z: number) {
  if (!Number.isFinite(z)) return;
  layout.zoom = Math.min(1.5, Math.max(0.6, Math.round(z * 100) / 100));
  save();
  if (inTauri) {
    const { getCurrentWebview } = await import("@tauri-apps/api/webview");
    await getCurrentWebview().setZoom(layout.zoom);
  } else {
    (document.documentElement.style as CSSStyleDeclaration & { zoom: string }).zoom = String(layout.zoom);
  }
  relayout();
}

/** Ctrl − / Ctrl = / Ctrl 0, like a browser. */
export function installZoomKeys() {
  void setZoom(layout.zoom);
  window.addEventListener("keydown", (e) => {
    if (!(e.ctrlKey || e.metaKey) || e.altKey) return;
    const i = ZOOMS.findIndex((z) => z >= layout.zoom - 1e-6);
    if (e.key === "-" || e.key === "_") void setZoom(ZOOMS[Math.max(0, (i < 0 ? ZOOMS.length : i) - 1)]);
    else if (e.key === "=" || e.key === "+") void setZoom(ZOOMS[Math.min(ZOOMS.length - 1, ZOOMS[i] === layout.zoom ? i + 1 : i)]);
    else if (e.key === "0") void setZoom(1);
    else return;
    e.preventDefault();
  });
}
