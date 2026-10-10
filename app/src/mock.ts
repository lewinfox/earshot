// A stand-in for the Rust core when the UI runs in a plain browser. It is only good
// enough to exercise the UI: steady demo grids, play/cue/jump/loop/sync, no audio.

import type { DeckState, EngineState, GridDto, LoadedTrack, TrackSource } from "./types";

type Listener = (p: unknown) => void;
const listeners = new Map<string, Listener[]>();

export function mockListen(event: string, cb: Listener) {
  listeners.set(event, [...(listeners.get(event) ?? []), cb]);
}

function emit(event: string, payload: unknown) {
  for (const cb of listeners.get(event) ?? []) cb(payload);
}

interface MockDeck extends DeckState {
  grid: GridDto | null;
  waveform: Uint8Array;
}

function emptyDeck(): MockDeck {
  return {
    position: 0, duration: 0, playing: false, speed: 1, rate: 0, bpm: null, sync: false,
    loop_in: null, loop_out: null, loop_active: false, cue_point: 0, hotcues: Array(8).fill(null),
    volume: 1, pfl: false, generation: 0, grid: null, waveform: new Uint8Array(),
  };
}

const decks = [emptyDeck(), emptyDeck()];
let crossfader = 0;
let generation = 0;
const jump = [4, 4];
const loopSize = [4, 4];

function beatAt(d: MockDeck, t: number) {
  const g = d.grid;
  if (!g) return t * 2;
  const p = 60 / bpmOf(d);
  return (t - g.times[0]) / p;
}
function timeAt(d: MockDeck, b: number) {
  const g = d.grid;
  if (!g) return b / 2;
  return g.times[0] + b * (60 / bpmOf(d));
}
function bpmOf(d: MockDeck) {
  const g = d.grid!;
  return (60 * (g.times.length - 1)) / (g.times[g.times.length - 1] - g.times[0]);
}

let last = performance.now();
setInterval(() => {
  const now = performance.now();
  const dt = (now - last) / 1000;
  last = now;
  for (let i = 0; i < 2; i++) {
    const d = decks[i];
    const o = decks[1 - i];
    d.speed = d.sync && o.grid && d.grid && o.playing ? (bpmOf(o) * o.speed) / bpmOf(d) : 1 + d.rate;
    if (d.playing) {
      d.position += dt * d.speed;
      if (d.loop_active && d.loop_in != null && d.loop_out != null && d.position >= d.loop_out) {
        d.position -= d.loop_out - d.loop_in;
      }
      if (d.position >= d.duration) {
        d.position = d.duration;
        d.playing = false;
      }
    }
    d.bpm = d.grid ? bpmOf(d) * d.speed : null;
  }
  const state: EngineState = {
    decks: decks.map(({ grid: _g, waveform: _w, ...rest }) => ({ ...rest })),
    crossfader,
    quantize: true,
    audio: { callbacks_per_second: 172, slow_callbacks: 0, xruns: 0, max_callback_ms: 0.2, budget_ms: 5.8, block_frames: 256, sample_rate: 44100 },
  };
  emit("engine-state", state);
}, 16);

function demoGrid(bpm: number, seconds: number): GridDto {
  const times: number[] = [];
  const positions: number[] = [];
  for (let i = 0; i * (60 / bpm) <= seconds; i++) {
    times.push(i * (60 / bpm));
    positions.push((i % 4) + 1);
  }
  return { times, positions, source: "demo (mock)" };
}

function demoWave(bpm: number, seconds: number): Uint8Array {
  const bins = Math.ceil(seconds * 150);
  const out = new Uint8Array(bins * 4);
  for (let i = 0; i < bins; i++) {
    const t = i / 150;
    const ph = ((t * bpm) / 60) % 1;
    const kick = Math.exp(-ph * 8);
    const hat = Math.exp(-(((ph + 0.5) % 1) * 30));
    out[i * 4] = 60 + 190 * Math.max(kick, hat * 0.4);
    out[i * 4 + 1] = 255 * kick;
    out[i * 4 + 2] = 120;
    out[i * 4 + 3] = 255 * hat;
  }
  return out;
}

export async function mockInvoke<T>(cmd: string, a: Record<string, any>): Promise<T> {
  const r = (v: unknown) => v as T;
  switch (cmd) {
    case "audio_info":
    case "start_audio":
      return r({ device: "browser mock", sample_rate: 44100, channels: 4, buffer_frames: 256, sample_format: "F32", null: true });
    case "midi_connect":
      return r([]);
    case "list_audio_devices":
      return r([]);
    case "scan_folder":
      return r([]);
    case "save_report":
      console.log("report", a.name, a.json);
      return r(`(browser) ${a.name}.json`);
    case "load_track": {
      const src = a.source as TrackSource;
      const bpm = src.kind === "demo" ? src.bpm : 124;
      const seconds = src.kind === "demo" ? src.seconds ?? 240 : 240;
      const d = decks[a.deck];
      Object.assign(d, emptyDeck(), {
        duration: seconds, grid: demoGrid(bpm, seconds), waveform: demoWave(bpm, seconds),
        generation: ++generation, volume: d.volume, pfl: d.pfl, sync: d.sync, rate: d.rate,
      });
      const t: LoadedTrack = {
        title: src.kind === "demo" ? src.title ?? `Demo ${bpm}` : src.path, artist: "mock",
        duration: seconds, sample_rate: 44100, grid: d.grid, waveform_bins_per_second: 150,
      };
      return r(t);
    }
    case "get_waveform":
      return r(decks[a.deck].waveform.buffer);
    case "edit_grid":
      return r(decks[a.deck].grid);
    case "control":
      mockControl(a.group, a.key, a.value);
      return r(null);
  }
  throw new Error(`mock: unknown command ${cmd}`);
}

function mockControl(group: string, key: string, value: number) {
  if (group === "[Library]" || group === "[Suggest]" || key === "LoadSelectedTrack") {
    emit("ui-control", { group, key, value });
    return;
  }
  if (group === "[Master]") {
    if (key === "crossfader") crossfader = value;
    return;
  }
  const i = group === "[Channel2]" ? 1 : 0;
  const d = decks[i];
  const on = value > 0;
  const hc = key.match(/^hotcue_(\d)_(activate|clear)$/);
  if (hc) {
    const s = +hc[1] - 1;
    if (!on) return;
    if (hc[2] === "clear") d.hotcues[s] = null;
    else if (d.hotcues[s] == null) d.hotcues[s] = d.position;
    else d.position = d.hotcues[s]!;
    return;
  }
  switch (key) {
    case "play": d.playing = on; break;
    case "cue_default":
      if (on && d.playing) { d.playing = false; d.position = d.cue_point; }
      else if (on) d.cue_point = d.position;
      break;
    case "sync_enabled": d.sync = on; break;
    case "pfl": d.pfl = on; break;
    case "volume": d.volume = value; break;
    case "rate": d.rate = value * 0.08; break;
    case "beatjump_forward": if (on) d.position = timeAt(d, beatAt(d, d.position) + jump[i]); break;
    case "beatjump_backward": if (on) d.position = Math.max(0, timeAt(d, beatAt(d, d.position) - jump[i])); break;
    case "beatjump_size": jump[i] = value; break;
    case "beatloop_size": loopSize[i] = value; break;
    case "beatloop_activate":
      if (!on) break;
      if (d.loop_active) d.loop_active = false;
      else {
        d.loop_in = timeAt(d, Math.floor(beatAt(d, d.position) + 0.05));
        d.loop_out = timeAt(d, beatAt(d, d.loop_in) + loopSize[i]);
        d.loop_active = true;
      }
      break;
    case "reloop_toggle": if (on && d.loop_in != null && d.loop_out != null) d.loop_active = !d.loop_active; break;
    case "playposition": d.position = value * d.duration; break;
  }
}
