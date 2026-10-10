// Track library: Camelot keys, a synthetic library for the benchmark, library tracks, search.

import type { LibTrack, Track } from "./types";

// ---- Camelot -----------------------------------------------------------------------

export interface Camelot {
  n: number; // 1..12
  minor: boolean; // 'A'
}

const MAJOR_BY_PITCH = ["8B", "3B", "10B", "5B", "12B", "7B", "2B", "9B", "4B", "11B", "6B", "1B"]; // C, Db, D …
const PITCH: Record<string, number> = { C: 0, D: 2, E: 4, F: 5, G: 7, A: 9, B: 11 };

const parsed = new Map<string, Camelot | null>();

/** Parse "8A", "Am", "C#m", "Ebmin", "F#" or "F# major" into Camelot. Memoised: scoring
 * calls this for every candidate, and a library only has a couple of dozen key strings. */
export function parseKey(raw: string | null | undefined): Camelot | null {
  if (!raw) return null;
  let k = parsed.get(raw);
  if (k === undefined) {
    k = parseKeyUncached(raw);
    parsed.set(raw, k);
  }
  return k;
}

function parseKeyUncached(raw: string): Camelot | null {
  const s = raw.trim();
  const cam = s.match(/^(\d{1,2})\s*([ABab])$/);
  if (cam) {
    const n = +cam[1];
    return n >= 1 && n <= 12 ? { n, minor: cam[2].toUpperCase() === "A" } : null;
  }
  const mus = s.match(/^([A-Ga-g])([#b♯♭]?)\s*(m|min|minor|maj|major)?$/);
  if (!mus) return null;
  let pitch = PITCH[mus[1].toUpperCase()];
  if (mus[2] === "#" || mus[2] === "♯") pitch += 1;
  if (mus[2] === "b" || mus[2] === "♭") pitch -= 1;
  pitch = (pitch + 12) % 12;
  const minor = !!mus[3] && mus[3].startsWith("m") && !mus[3].startsWith("maj");
  // A minor key's Camelot number is that of its relative major (3 semitones up).
  const major = MAJOR_BY_PITCH[(pitch + (minor ? 3 : 0)) % 12];
  return { n: parseInt(major), minor };
}

export function keyName(k: Camelot): string {
  return `${k.n}${k.minor ? "A" : "B"}`;
}

/** Shift a key by `semitones` (e.g. pitching a track up without keylock). */
export function transpose(k: Camelot, semitones: number): Camelot {
  const n = ((((k.n - 1 + semitones * 7) % 12) + 12) % 12) + 1;
  return { n, minor: k.minor };
}

/** Distance in steps round the wheel, 0..6. */
export function wheelDistance(a: Camelot, b: Camelot): number {
  const d = Math.abs(a.n - b.n) % 12;
  return Math.min(d, 12 - d);
}

// ---- synthetic library ---------------------------------------------------------------

function rng(seed: number) {
  let s = seed >>> 0 || 1;
  return () => {
    s ^= s << 13;
    s ^= s >>> 17;
    s ^= s << 5;
    return (s >>> 0) / 4294967296;
  };
}

function gauss(r: () => number) {
  return Math.sqrt(-2 * Math.log(r() + 1e-12)) * Math.cos(2 * Math.PI * r());
}

const GENRES = [
  { name: "deep house", bpm: 122, energy: 0.45 },
  { name: "tech house", bpm: 126, energy: 0.65 },
  { name: "techno", bpm: 132, energy: 0.8 },
  { name: "minimal", bpm: 125, energy: 0.5 },
  { name: "disco", bpm: 118, energy: 0.6 },
  { name: "breaks", bpm: 130, energy: 0.75 },
  { name: "dnb", bpm: 172, energy: 0.85 },
  { name: "downtempo", bpm: 100, energy: 0.3 },
  { name: "acid", bpm: 128, energy: 0.8 },
  { name: "garage", bpm: 133, energy: 0.65 },
  { name: "afro", bpm: 120, energy: 0.6 },
  { name: "electro", bpm: 127, energy: 0.7 },
];
const WORDS_A = ["Velvet", "Neon", "Hollow", "Silver", "Midnight", "Static", "Golden", "Liquid", "Broken", "Paper", "Echo", "Solar", "Quiet", "Electric", "Distant", "Crimson"];
const WORDS_B = ["Signal", "Garden", "Motion", "Harbour", "Circuit", "Season", "Mirror", "Engine", "Shadow", "River", "Pulse", "Machine", "Theory", "Ritual", "Window", "Tide"];
const ARTISTS = ["Kalimba Sound", "Orla Vance", "Dub Theory", "Night Ferry", "Marlo", "The Pressure Unit", "Sefa K", "Lumen Collective", "Haze & Ada", "Polyline", "Ruth Okafor", "Second Shift", "Ivo", "Coast Patrol", "Mira Lane", "Tape Loop Club"];
export const VIBE_DIMS = 16;

/** A plausible fake library: genre clusters with their own tempo, energy and vibe. */
export function syntheticLibrary(n: number, firstId: number, seed = 7): Track[] {
  const r = rng(seed);
  const centres = GENRES.map(() => {
    const v = new Float32Array(VIBE_DIMS);
    for (let i = 0; i < VIBE_DIMS; i++) v[i] = gauss(r);
    return v;
  });
  const tracks: Track[] = [];
  for (let i = 0; i < n; i++) {
    const g = Math.floor(r() * GENRES.length);
    const genre = GENRES[g];
    const bpm = Math.round((genre.bpm + gauss(r) * 2.5) * 10) / 10;
    // One in ten is a live recording whose tempo drifts.
    const drift = r() < 0.1 ? (r() - 0.5) * 6 : 0;
    const key = { n: 1 + Math.floor(r() * 12), minor: r() < 0.65 };
    const vibe = new Float32Array(VIBE_DIMS);
    let norm = 0;
    for (let k = 0; k < VIBE_DIMS; k++) {
      vibe[k] = centres[g][k] + gauss(r) * 0.7;
      norm += vibe[k] * vibe[k];
    }
    norm = Math.sqrt(norm);
    for (let k = 0; k < VIBE_DIMS; k++) vibe[k] /= norm;
    const title = `${WORDS_A[Math.floor(r() * WORDS_A.length)]} ${WORDS_B[Math.floor(r() * WORDS_B.length)]}`;
    const keyStr = keyName(key);
    tracks.push({
      id: firstId + i,
      title,
      artist: ARTISTS[Math.floor(r() * ARTISTS.length)],
      bpm,
      key: keyStr,
      energy: Math.min(1, Math.max(0, genre.energy + gauss(r) * 0.12)),
      vibe,
      cluster: g,
      source: {
        kind: "demo",
        bpm,
        end_bpm: drift ? Math.round((bpm + drift) * 10) / 10 : undefined,
        key: keyStr,
        seed: i + 1,
        seconds: 240,
        title: drift ? `${title} (live)` : title,
      },
    });
    if (drift) tracks[tracks.length - 1].title += " (live)";
  }
  return tracks;
}

const ANALYSED_SOURCES = new Set(["analysis", "user", "tempo-lock sidecar"]);

export function fromLibrary(l: LibTrack): Track {
  const k = parseKey(l.key);
  return {
    id: l.id,
    title: l.title,
    artist: l.artist,
    bpm: l.bpm ?? l.tag_bpm,
    key: k ? keyName(k) : null,
    energy: 0.5,
    energyRaw: l.energy,
    vibe: null, // CLAP embeddings come from the worker (not built yet)
    cluster: -1,
    source: { kind: "track", id: l.id },
    path: l.path,
    duration: l.duration,
    analysed: l.grid_source != null && ANALYSED_SOURCES.has(l.grid_source),
    analysis: null,
    analysisError: null,
  };
}

/** Set `energy` to each track's rank (0..1) by raw energy; tracks without one get 0.5. */
export function rankEnergy(tracks: Track[]) {
  const known = tracks.filter((t) => t.energyRaw != null).sort((a, b) => a.energyRaw! - b.energyRaw!);
  known.forEach((t, i) => (t.energy = known.length > 1 ? i / (known.length - 1) : 0.5));
}

// ---- search -------------------------------------------------------------------------

/** Free text plus `bpm:120-126`, `bpm:124`, `key:8A` (also `key:Am`). */
export function search(tracks: Track[], query: string): Track[] {
  const words: string[] = [];
  let bpmLo = -Infinity;
  let bpmHi = Infinity;
  let key: string | null = null;
  for (const tok of query.trim().split(/\s+/).filter(Boolean)) {
    const b = tok.match(/^bpm:(\d+(?:\.\d+)?)(?:-(\d+(?:\.\d+)?))?$/i);
    const k = tok.match(/^key:(.+)$/i);
    if (b) {
      bpmLo = +b[1] - (b[2] ? 0 : 0.5);
      bpmHi = b[2] ? +b[2] : +b[1] + 0.5;
    } else if (k) {
      const c = parseKey(k[1]);
      key = c ? keyName(c) : k[1].toUpperCase();
    } else words.push(tok.toLowerCase());
  }
  return tracks.filter((t) => {
    if (key && t.key !== key) return false;
    if (bpmLo > -Infinity && (t.bpm == null || t.bpm < bpmLo || t.bpm > bpmHi)) return false;
    if (words.length) {
      const hay = `${t.title} ${t.artist}`.toLowerCase();
      return words.every((w) => hay.includes(w));
    }
    return true;
  });
}
