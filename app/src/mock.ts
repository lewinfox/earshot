// A stand-in for the Rust core when the UI runs in a plain browser. It is only good
// enough to exercise the UI: steady demo grids, play/cue/jump/loop/sync, no audio.

import type { DeckState, EngineState, GridDto, LibTrack, LoadedTrack, TrackSource } from "./types";

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
  detail: Uint8Array;
}

function emptyDeck(): MockDeck {
  return {
    position: 0, duration: 0, playing: false, speed: 1, rate: 0, bpm: null, sync: false,
    loop_in: null, loop_out: null, loop_active: false, cue_point: 0, hotcues: Array(8).fill(null),
    volume: 1, pfl: false, generation: 0, grid: null, waveform: new Uint8Array(), detail: new Uint8Array(),
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
    d.bpm = d.grid ? bpmOf(d) * (1 + d.rate) : null;
  }
  const state: EngineState = {
    decks: decks.map(({ grid: _g, waveform: _w, detail: _d, ...rest }) => ({ ...rest })),
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

/** Min/max per 1/2000 s of a synthetic signal: a kick (a falling sine) on each beat,
 * a hi-hat (noise) on the off-beat and a 55 Hz bassline. */
function demoDetail(bpm: number, seconds: number): Uint8Array {
  const sr = 22050;
  const per = sr / 2000;
  const bins = Math.ceil(seconds * 2000);
  const out = new Uint8Array(bins * 2);
  const beat = 60 / bpm;
  let phase = 0;
  let noise = 1;
  for (let j = 0; j < bins; j++) {
    let lo = 1;
    let hi = -1;
    for (let k = 0; k < per; k++) {
      const t = (j * per + k) / sr;
      const pb = t % beat;
      phase += (2 * Math.PI * (45 + 120 * Math.exp(-pb * 25))) / sr;
      const kick = Math.sin(phase) * Math.exp(-pb * 9) * 0.8;
      const ph = (t + beat / 2) % beat;
      noise = (noise * 16807) % 2147483647;
      const hat = (noise / 2147483647 - 0.5) * Math.exp(-ph * 60) * 0.5;
      const bass = Math.sin(2 * Math.PI * 55 * t) * 0.25 * (pb > beat * 0.3 ? 1 : 0.2);
      const x = Math.max(-1, Math.min(1, kick + hat + bass));
      lo = Math.min(lo, x);
      hi = Math.max(hi, x);
    }
    out[j * 2] = Math.round(128 + 127 * lo);
    out[j * 2 + 1] = Math.round(128 + 127 * hi);
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
    case "library_folders":
      return r(mockFolders);
    case "add_library_folder":
      if (!mockFolders.includes(a.path)) mockFolders.push(a.path);
      return r(mockFolders);
    case "remove_library_folder":
      mockFolders = mockFolders.filter((f) => f !== a.path);
      return r(mockFolders);
    case "list_subdirs":
      return r(a.path === "/music" ? [{ name: "house", path: "/music/house", has_children: false }, { name: "disco", path: "/music/disco", has_children: false }] : []);
    case "scan_library":
      return r(mockFolders.includes("/music") ? MOCK_TRACKS : []);
    case "waveform_previews": {
      // Every other track has one: intro, build, drop, breakdown, drop, outro.
      const ids = (a.ids as number[] | null) ?? MOCK_TRACKS.filter((t) => t.id % 2 === 0).map((t) => t.id);
      const out = new Uint8Array(ids.length * (8 + 512));
      const view = new DataView(out.buffer);
      ids.forEach((id, j) => {
        const o = j * 520;
        view.setBigInt64(o, BigInt(id), true);
        for (let i = 0; i < 128; i++) {
          const x = i / 128;
          const level = x < 0.1 ? 0.35 : x < 0.25 ? 0.6 : x < 0.5 ? 0.95 : x < 0.6 ? 0.3 : x < 0.9 ? 1 : 0.4;
          const kick = x > 0.25 && x < 0.5 ? 1 : x > 0.6 && x < 0.9 ? 1 : 0.3;
          out.set([255 * level * (0.85 + 0.15 * Math.sin(i * (id + 3))), 200 * kick, 120, 140 * (1 - kick) + 60], o + 8 + i * 4);
        }
      });
      return r(out.buffer);
    }
    case "analyse_tracks":
      for (const id of a.ids as number[]) mockAnalyse(id);
      return r(null);
    case "save_report":
      console.log("report", a.name, a.json);
      return r(`(browser) ${a.name}.json`);
    case "load_track": {
      const src = a.source as TrackSource;
      const lib = src.kind === "track" ? MOCK_TRACKS.find((t) => t.id === src.id) : undefined;
      const bpm = src.kind === "demo" ? src.bpm : lib?.tag_bpm ?? 124;
      const seconds = src.kind === "demo" ? src.seconds ?? 240 : 240;
      const d = decks[a.deck];
      Object.assign(d, emptyDeck(), {
        duration: seconds, grid: demoGrid(bpm, seconds), waveform: demoWave(bpm, seconds), detail: demoDetail(bpm, seconds),
        generation: ++generation, volume: d.volume, pfl: d.pfl, sync: d.sync, rate: d.rate,
      });
      const t: LoadedTrack = {
        title: src.kind === "demo" ? src.title ?? `Demo ${bpm}` : lib?.title ?? "?", artist: lib?.artist ?? "mock",
        duration: seconds, sample_rate: 44100, grid: d.grid, waveform_bins_per_second: 150, waveform_detail_per_second: 2000,
      };
      return r(t);
    }
    case "get_waveform":
      return r(decks[a.deck].waveform.buffer);
    case "get_waveform_detail":
      return r(decks[a.deck].detail.buffer);
    case "edit_grid":
      return r(decks[a.deck].grid);
    case "control":
      mockControl(a.group, a.key, a.value);
      return r(null);
  }
  throw new Error(`mock: unknown command ${cmd}`);
}

let mockFolders = ["/music"];
const MOCK_WORDS = ["Night", "Velvet", "Static", "Golden", "Echo", "Neon", "Liquid", "Crimson", "Quiet", "Hollow"];
const MOCK_WORDS2 = ["Drive", "Theory", "River", "Window", "Harbour", "Pulse", "Season", "Machine", "Garden", "Signal"];
const MOCK_ARTISTS = ["Lumen Collective", "The Pressure Unit", "Mira Lane", "Haze & Ada", "Sefa K", "Orla Vance", "Dub Theory"];
const MOCK_TRACKS: LibTrack[] = Array.from({ length: 40 }, (_, i) => {
  const house = i % 3 !== 2;
  const title = `${MOCK_WORDS[i % 10]} ${MOCK_WORDS2[(i * 7) % 10]}`;
  return {
    id: i + 1,
    path: `/music/${house ? "house" : "disco"}/${title}.mp3`,
    title,
    artist: MOCK_ARTISTS[(i * 3) % MOCK_ARTISTS.length],
    duration: 280 + ((i * 37) % 120),
    bpm: null,
    tag_bpm: house ? 122 + (i % 6) : 116 + (i % 4),
    key: `${1 + ((i * 5) % 12)}${i % 4 === 0 ? "B" : "A"}`,
    grid_source: null,
    energy: -14 + ((i * 13) % 10),
  };
});

function mockAnalyse(id: number) {
  const ev = (state: string, extra = {}) =>
    emit("analysis", { track_id: id, state, progress: null, stage: null, bpm: null, grid_source: null, error: null, ...extra });
  ev("queued");
  for (let i = 0; i <= 10; i++) setTimeout(() => ev("running", { progress: i / 10, stage: i < 8 ? "finding beats" : "refining beats" }), 300 + i * 400);
  setTimeout(() => ev("done", { bpm: MOCK_TRACKS.find((x) => x.id === id)?.tag_bpm ?? null, grid_source: "analysis" }), 4800);
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
