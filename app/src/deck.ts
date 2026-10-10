// One deck, laid out like Rekordbox's performance view: header, overview, hot cue pads,
// transport and a jog display. Its scrolling waveform lives in the stacked row above.
// Every button goes
// through the control bus (`control()`), exactly like the keyboard and MIDI do.

import { control, invoke } from "./api";
import { effectiveKey } from "./suggest";
import { keyName } from "./library";
import type { AnalysisEvent, DeckState, GridDto, LoadedTrack, Track } from "./types";
import { HOTCUE_COLOURS, lowerBound, OverviewView, WaveformView } from "./waveform";

/** Minutes, seconds and tenths, like 02:44.7. */
const fmt = (s: number) => {
  if (!isFinite(s)) return "–:––.–";
  const tenths = Math.floor(Math.abs(s) * 10);
  const m = Math.floor(tenths / 600);
  const r = Math.floor(tenths / 10) % 60;
  return `${s < 0 ? "-" : ""}${m.toString().padStart(2, "0")}:${r.toString().padStart(2, "0")}.${tenths % 10}`;
};

/**
 * Seconds of real time visible either side of the playhead, shared by both decks.
 * Each deck shows `halfWindow * speed` seconds of its track, so both waveforms scroll
 * at the same screen speed and beat markers are spaced by the tempo you hear: two
 * beatmatched decks line up even when their tracks' own BPMs differ.
 */
let halfWindow = 4;

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
  private wave: WaveformView | null = null;
  private overview: OverviewView;
  private barLabel: HTMLElement;
  /** Bar number of each grid beat (counting downbeats), for the bar.beat readout. */
  private bars: number[] = [];
  private els: Record<string, HTMLElement> = {};

  constructor(
    readonly index: number,
    private root: HTMLElement,
    waveSlot: HTMLElement,
    onDrop: (deck: number, trackId: number) => void,
  ) {
    this.group = `[Channel${index + 1}]`;
    root.innerHTML = `
      <div class="deck-head">
        <span class="deck-num">${index + 1}</span>
        <div class="deck-id">
          <div class="deck-title" data-el="title">Drop a track here</div>
          <div class="deck-sub">
            <span class="deck-artist" data-el="artist"></span>
            <span data-el="tagbpm"></span>
            <span class="key" data-el="key"></span>
            <span class="grid-src" data-el="gridsrc"></span>
          </div>
        </div>
        <div class="deck-times">
          <span class="remain" data-el="remain">–:––.–</span>
          <span data-el="time">–:––.–</span>
        </div>
        <div class="deck-sync">
          <button data-ctl="sync_enabled" data-toggle>BEAT SYNC</button>
          <button data-ctl="pfl" data-toggle title="Headphone cue">🎧</button>
        </div>
      </div>
      <div class="wave" data-el="wave"><canvas data-el="gl"></canvas><canvas data-el="ov"></canvas></div>
      <div class="overview" data-el="overview"><canvas data-el="ovc"></canvas><i class="ov-prog" data-el="prog"></i></div>
      <div class="deck-body">
        <div class="pads" data-el="pads">
          ${"ABCDEFGH"
            .split("")
            .map((l, i) => `<button data-hotcue="${i + 1}" title="Click: set/jump · Shift-click: clear"><b>${l}</b><span></span></button>`)
            .join("")}
        </div>
        <div class="transport">
          <div class="tgrp" title="Beat jump">
            <button data-ctl="beatjump_backward" title="Beat jump back">‹</button>
            <select data-el="jump" title="Beat jump size">
              ${[1, 2, 4, 8, 16, 32].map((n) => `<option ${n === 4 ? "selected" : ""}>${n}</option>`).join("")}
            </select>
            <button data-ctl="beatjump_forward" title="Beat jump forward">›</button>
          </div>
          <div class="tgrp" title="Loop">
            <select data-el="loopsize" title="Auto loop size (beats)">
              ${[1, 2, 4, 8, 16].map((n) => `<option ${n === 4 ? "selected" : ""}>${n}</option>`).join("")}
            </select>
            <button data-ctl="beatloop_activate" data-el="autoloop" title="Auto loop on/off">LOOP</button>
            <button data-ctl="loop_halve" title="Halve loop">½</button>
            <button data-ctl="loop_double" title="Double loop">×2</button>
          </div>
          <div class="tgrp">
            <button data-ctl="loop_in">IN</button>
            <button data-ctl="loop_out">OUT</button>
            <button data-ctl="reloop_toggle" data-el="reloop" title="Reloop / exit">EXIT</button>
          </div>
          <div class="tgrp round-row">
            <button class="round cue" data-ctl="cue_default" data-hold>CUE</button>
            <button class="round play" data-ctl="play" data-toggle title="Play / pause">▶</button>
          </div>
        </div>
        <div class="jog">
          <div class="dial" data-el="dial">
            <i class="dot" data-el="dot"></i>
            <span class="dial-bpm" data-el="bpm">–</span>
            <span class="dial-rate" data-el="rate">0.0%</span>
          </div>
        </div>
      </div>
      <div class="deck-tools">
        <span class="grp pitch">
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
    waveSlot.append(this.els.wave); // the scrolling waveforms are stacked above the decks
    this.barLabel = document.createElement("span");
    this.barLabel.className = "bar-label";
    this.els.wave.append(this.barLabel);
    try {
      this.wave = new WaveformView(this.els.gl as HTMLCanvasElement, this.els.ov as HTMLCanvasElement);
    } catch (e) {
      this.els.title.textContent = `WebGL2 unavailable: ${e}`;
    }
    this.overview = new OverviewView(this.els.ovc as HTMLCanvasElement);
    this.bindControls();

    // Drop targets for tracks dragged from the library, suggestions or graph.
    for (const el of [root, waveSlot]) {
      el.addEventListener("dragover", (e) => {
        e.preventDefault();
        el.classList.add("drop");
      });
      el.addEventListener("dragleave", () => el.classList.remove("drop"));
      el.addEventListener("drop", (e) => {
        e.preventDefault();
        el.classList.remove("drop");
        const id = Number(e.dataTransfer?.getData("text/earshot-track"));
        if (Number.isFinite(id)) onDrop(index, id);
      });
    }
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
    const loopsize = this.els.loopsize as HTMLSelectElement;
    loopsize.addEventListener("change", () => {
      send("beatloop_size", +loopsize.value);
      loopsize.blur();
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
      halfWindow = Math.min(30, Math.max(0.5, halfWindow * (e.deltaY > 0 ? 1.15 : 1 / 1.15)));
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
    this.els.title.textContent = loaded.title;
    this.els.artist.textContent = loaded.artist;
    this.els.tagbpm.textContent = track.bpm != null ? track.bpm.toFixed(2) : "";
    this.setGrid(loaded.grid);
    const [buf, fine] = await Promise.all([
      invoke<ArrayBuffer>("get_waveform", { deck: this.index }),
      invoke<ArrayBuffer>("get_waveform_detail", { deck: this.index }),
    ]);
    const data = new Uint8Array(buf);
    this.wave?.setWaveform(data, loaded.waveform_bins_per_second, new Uint8Array(fine), loaded.waveform_detail_per_second);
    this.overview.setWaveform(data);
  }

  /** Analysis progress for the loaded track: a bar under the overview and a label. */
  showAnalysis(ev: AnalysisEvent) {
    const bar = this.els.prog;
    if (ev.state === "running" || ev.state === "queued") {
      const p = Math.round((ev.progress ?? 0) * 100);
      bar.style.width = `${p}%`;
      bar.style.display = "block";
      this.els.gridsrc.textContent = ev.state === "queued" ? "waiting to analyse…" : `${ev.stage ?? "analysing"}… ${p}%`;
      this.els.gridsrc.classList.add("busy");
    } else {
      bar.style.display = "none";
      this.els.gridsrc.classList.remove("busy");
      if (ev.state === "error") {
        this.els.gridsrc.textContent = "analysis failed";
        this.els.gridsrc.title = ev.error ?? "";
      } else this.setGrid(this.grid);
    }
  }

  setGrid(g: GridDto | null) {
    this.grid = g;
    let bar = 0;
    this.bars = g ? g.positions.map((p) => (p === 1 ? ++bar : Math.max(bar, 1))) : [];
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
    this.root.classList.toggle("ending", s.playing && s.duration - s.position < 30);
    const pitch = this.els.pitch as HTMLInputElement;
    if (document.activeElement !== pitch) pitch.value = String(s.rate / 0.08);
    this.root.querySelector('[data-ctl="play"]')!.classList.toggle("lit", s.playing);
    this.root.querySelector('[data-ctl="sync_enabled"]')!.classList.toggle("lit", s.sync);
    this.root.querySelector('[data-ctl="pfl"]')!.classList.toggle("lit", s.pfl);
    this.els.reloop.classList.toggle("lit", s.loop_active);
    this.els.autoloop.classList.toggle("lit", s.loop_active);
    this.els.pads.querySelectorAll("button").forEach((b, i) => {
      const t = s.hotcues[i];
      b.classList.toggle("set", t != null);
      b.style.setProperty("--c", HOTCUE_COLOURS[i]);
      b.querySelector("span")!.textContent = t != null ? fmt(t) : "";
    });
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
      halfWindow: halfWindow * (s?.speed ?? 1),
      grid: this.grid,
      cuePoint: s?.cue_point ?? 0,
      hotcues: s?.hotcues ?? [],
      loopIn: s?.loop_in ?? null,
      loopOut: s?.loop_out ?? null,
      loopActive: s?.loop_active ?? false,
    });
    this.overview.draw(this.shown, s?.duration ?? 0, s?.loop_in ?? null, s?.loop_out ?? null, s?.cue_point ?? 0, s?.hotcues ?? []);
    // The jog dial's marker turns like a record at 33⅓ rpm (1.8 s a turn).
    this.els.dot.style.transform = `rotate(${((this.shown / 1.8) % 1) * 360}deg)`;
    this.barLabel.textContent = this.barBeat(this.shown);
  }

  private barBeat(t: number): string {
    const g = this.grid;
    if (!g || g.times.length === 0 || t < g.times[0]) return "";
    const i = lowerBound(g.times, t + 1e-6) - 1;
    return `${this.bars[i]}.${g.positions[i]} Bars`;
  }
}
