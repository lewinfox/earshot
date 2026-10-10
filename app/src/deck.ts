// One deck: header, scrolling waveform, overview and controls. Every button goes
// through the control bus (`control()`), exactly like the keyboard and MIDI do.

import { control, invoke } from "./api";
import { effectiveKey } from "./suggest";
import { keyName } from "./library";
import type { DeckState, GridDto, LoadedTrack, Track } from "./types";
import { OverviewView, WaveformView } from "./waveform";

const fmt = (s: number) => {
  if (!isFinite(s)) return "–:––";
  const m = Math.floor(Math.abs(s) / 60);
  const r = Math.floor(Math.abs(s) % 60);
  return `${s < 0 ? "-" : ""}${m}:${r.toString().padStart(2, "0")}`;
};

export class DeckView {
  readonly group: string;
  track: Track | null = null;
  grid: GridDto | null = null;
  state: DeckState | null = null;
  /** Interpolated playhead used for drawing. */
  shown = 0;
  private received = 0;
  private lastFrame = 0;
  private generation = -1;
  private halfWindow = 4;
  private wave: WaveformView | null = null;
  private overview: OverviewView;
  private els: Record<string, HTMLElement> = {};

  constructor(
    readonly index: number,
    private root: HTMLElement,
    onDrop: (deck: number, trackId: number) => void,
  ) {
    this.group = `[Channel${index + 1}]`;
    const letter = "AB"[index];
    root.innerHTML = `
      <div class="deck-head">
        <span class="deck-letter">${letter}</span>
        <span class="deck-title" data-el="title">Drop a track here</span>
        <span class="deck-bpm" data-el="bpm">–</span>
      </div>
      <div class="deck-meta">
        <span>key <span class="key" data-el="key">–</span></span>
        <span data-el="rate">0.0%</span>
        <span data-el="time">–:––</span>
        <span data-el="remain">–:––</span>
        <span class="grid-src" data-el="gridsrc"></span>
      </div>
      <div class="wave" data-el="wave"><canvas data-el="gl"></canvas><canvas data-el="ov"></canvas></div>
      <div class="overview" data-el="overview"><canvas data-el="ovc"></canvas></div>
      <div class="deck-controls">
        <span class="grp">
          <button class="big" data-ctl="cue_default" data-hold>CUE</button>
          <button class="big" data-ctl="play" data-toggle>PLAY</button>
          <button data-ctl="sync_enabled" data-toggle>SYNC</button>
          <button data-ctl="pfl" data-toggle title="Headphone cue">🎧</button>
        </span>
        <span class="grp">
          <button data-ctl="beatjump_backward" title="Beat jump back">◀</button>
          <select data-el="jump" title="Beat jump size">
            ${[1, 2, 4, 8, 16, 32].map((n) => `<option ${n === 4 ? "selected" : ""}>${n}</option>`).join("")}
          </select>
          <button data-ctl="beatjump_forward" title="Beat jump forward">▶</button>
        </span>
        <span class="grp">
          ${[1, 2, 4, 8, 16].map((n) => `<button data-ctl="beatloop_${n}_toggle">${n}</button>`).join("")}
          <button data-ctl="loop_in">IN</button>
          <button data-ctl="loop_out">OUT</button>
          <button data-ctl="reloop_toggle" data-el="reloop">RELOOP</button>
          <button data-ctl="loop_halve">½</button>
          <button data-ctl="loop_double">×2</button>
        </span>
      </div>
      <div class="pads" data-el="pads">
        ${[1, 2, 3, 4, 5, 6, 7, 8].map((n) => `<button data-hotcue="${n}" title="Click: set/jump · Shift-click: clear">${n}</button>`).join("")}
      </div>
      <div class="deck-controls">
        <span class="grp pitch" style="flex:1">
          <button data-ctl="rate_temp_down" data-hold title="Nudge slower">−</button>
          <input type="range" min="-1" max="1" step="0.001" value="0" data-el="pitch" title="Pitch ±8% (double-click to reset)" />
          <button data-ctl="rate_temp_up" data-hold title="Nudge faster">+</button>
        </span>
        <span class="grp" title="Beatgrid: whole-grid edits">
          <input type="number" step="0.01" min="40" max="300" data-el="setbpm" placeholder="BPM" />
          <button data-grid="nudge-">◁</button>
          <button data-grid="nudge+">▷</button>
          <button data-grid="downbeat" title="Make the beat nearest the playhead a 1">1</button>
          <button data-grid="halve">÷2</button>
          <button data-grid="double">×2</button>
        </span>
      </div>`;
    root.querySelectorAll<HTMLElement>("[data-el]").forEach((e) => (this.els[e.dataset.el!] = e));
    try {
      this.wave = new WaveformView(this.els.gl as HTMLCanvasElement, this.els.ov as HTMLCanvasElement);
    } catch (e) {
      this.els.title.textContent = `WebGL2 unavailable: ${e}`;
    }
    this.overview = new OverviewView(this.els.ovc as HTMLCanvasElement);
    this.bindControls();

    // Drop targets for tracks dragged from the library, suggestions or graph.
    root.addEventListener("dragover", (e) => {
      e.preventDefault();
      root.classList.add("drop");
    });
    root.addEventListener("dragleave", () => root.classList.remove("drop"));
    root.addEventListener("drop", (e) => {
      e.preventDefault();
      root.classList.remove("drop");
      const id = Number(e.dataTransfer?.getData("text/earshot-track"));
      if (Number.isFinite(id)) onDrop(index, id);
    });
  }

  private bindControls() {
    const send = (key: string, v: number) => control(this.group, key, v);
    this.root.querySelectorAll<HTMLButtonElement>("[data-ctl]").forEach((b) => {
      const key = b.dataset.ctl!;
      b.addEventListener("pointerdown", (e) => {
        e.preventDefault();
        if (b.hasAttribute("data-toggle")) send(key, this.isOn(key) ? 0 : 1);
        else send(key, 1);
      });
      b.addEventListener("pointerup", () => !b.hasAttribute("data-toggle") && send(key, 0));
      b.addEventListener("pointerleave", (e) => e.buttons && b.hasAttribute("data-hold") && send(key, 0));
    });
    this.root.querySelectorAll<HTMLButtonElement>("[data-hotcue]").forEach((b) => {
      const n = b.dataset.hotcue;
      b.addEventListener("pointerdown", (e) => {
        e.preventDefault();
        send(e.shiftKey ? `hotcue_${n}_clear` : `hotcue_${n}_activate`, 1);
      });
      b.addEventListener("pointerup", () => send(`hotcue_${n}_activate`, 0));
    });
    const pitch = this.els.pitch as HTMLInputElement;
    pitch.addEventListener("input", () => send("rate", +pitch.value));
    pitch.addEventListener("dblclick", () => {
      pitch.value = "0";
      send("rate", 0);
    });
    const jump = this.els.jump as HTMLSelectElement;
    jump.addEventListener("change", () => {
      send("beatjump_size", +jump.value);
      jump.blur(); // give the keyboard back to the decks
    });

    const edit = (op: Record<string, unknown>) =>
      invoke<GridDto>("edit_grid", { deck: this.index, op })
        .then((g) => this.setGrid(g))
        .catch((e) => console.warn("grid edit:", e));
    this.root.querySelectorAll<HTMLButtonElement>("[data-grid]").forEach((b) => {
      b.addEventListener("click", () => {
        const g = b.dataset.grid!;
        if (g === "nudge-") edit({ op: "nudge", ms: -10 });
        else if (g === "nudge+") edit({ op: "nudge", ms: 10 });
        else if (g === "downbeat") edit({ op: "set_downbeat" });
        else edit({ op: g });
      });
    });
    const setbpm = this.els.setbpm as HTMLInputElement;
    setbpm.addEventListener("change", () => {
      if (+setbpm.value > 0) edit({ op: "set_bpm", bpm: +setbpm.value });
      setbpm.blur();
    });

    // Scroll on the waveform to zoom; click the overview to seek.
    this.els.wave.addEventListener("wheel", (e) => {
      e.preventDefault();
      this.halfWindow = Math.min(30, Math.max(1, this.halfWindow * (e.deltaY > 0 ? 1.15 : 1 / 1.15)));
    });
    this.els.overview.addEventListener("pointerdown", (e) => {
      const r = this.els.overview.getBoundingClientRect();
      send("playposition", (e.clientX - r.left) / r.width);
    });
  }

  isOn(key: string): boolean {
    const s = this.state;
    if (!s) return false;
    return key === "play" ? s.playing : key === "sync_enabled" ? s.sync : key === "pfl" ? s.pfl : false;
  }

  async setTrack(track: Track, loaded: LoadedTrack) {
    this.track = track;
    this.els.title.innerHTML = "";
    this.els.title.append(loaded.title);
    const small = document.createElement("small");
    small.textContent = loaded.artist;
    this.els.title.append(small);
    this.setGrid(loaded.grid);
    const buf = await invoke<ArrayBuffer>("get_waveform", { deck: this.index });
    const data = new Uint8Array(buf);
    this.wave?.setWaveform(data, loaded.waveform_bins_per_second);
    this.overview.setWaveform(data);
  }

  setGrid(g: GridDto | null) {
    this.grid = g;
    this.els.gridsrc.textContent = g ? `grid: ${g.source}` : "no grid";
    this.els.gridsrc.title = g ? `${g.times.length} beats` : "";
  }

  update(s: DeckState, at: number) {
    const jumped = this.state == null || Math.abs(this.expected(at) - s.position) > 0.05 || s.generation !== this.generation;
    this.state = s;
    this.received = at;
    this.generation = s.generation;
    if (jumped) this.shown = s.position;

    this.els.bpm.textContent = s.bpm != null ? s.bpm.toFixed(2) : "–";
    this.els.rate.textContent = `${s.speed >= 1 ? "+" : ""}${((s.speed - 1) * 100).toFixed(1)}%`;
    const k = effectiveKey(this.track?.key ?? null, s.speed);
    this.els.key.textContent = k ? keyName(k) : this.track?.key ?? "–";
    this.els.time.textContent = fmt(s.position);
    this.els.remain.textContent = fmt(s.position - s.duration);
    const pitch = this.els.pitch as HTMLInputElement;
    if (document.activeElement !== pitch) pitch.value = String(s.rate / 0.08);
    this.root.querySelector('[data-ctl="play"]')!.classList.toggle("lit", s.playing);
    this.root.querySelector('[data-ctl="sync_enabled"]')!.classList.toggle("lit", s.sync);
    this.root.querySelector('[data-ctl="pfl"]')!.classList.toggle("lit", s.pfl);
    this.els.reloop.classList.toggle("lit", s.loop_active);
    this.els.pads.querySelectorAll("button").forEach((b, i) => b.classList.toggle("set", s.hotcues[i] != null));
  }

  /** Where the engine's playhead should be now, extrapolated from the last update. */
  private expected(now: number) {
    const s = this.state;
    if (!s) return 0;
    return s.playing ? s.position + ((now - this.received) / 1000) * s.speed : s.position;
  }

  frame(now: number) {
    const s = this.state;
    const dt = this.lastFrame ? (now - this.lastFrame) / 1000 : 0;
    this.lastFrame = now;
    if (s) {
      // Advance smoothly at the playback speed and ease towards the engine's clock,
      // so 60 Hz state updates with jitter still give a steady scroll.
      const target = this.expected(now);
      const next = s.playing ? this.shown + dt * s.speed : target;
      this.shown = Math.abs(target - next) > 0.05 ? target : next + (target - next) * 0.15;
    }
    this.wave?.draw({
      position: this.shown,
      halfWindow: this.halfWindow,
      grid: this.grid,
      cuePoint: s?.cue_point ?? 0,
      hotcues: s?.hotcues ?? [],
      loopIn: s?.loop_in ?? null,
      loopOut: s?.loop_out ?? null,
      loopActive: s?.loop_active ?? false,
    });
    this.overview.draw(this.shown, s?.duration ?? 0, s?.loop_in ?? null, s?.loop_out ?? null);
  }
}
