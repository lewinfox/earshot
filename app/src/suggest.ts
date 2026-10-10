// "What could I play next?" Score every candidate against a reference (the selected
// track, or the playing deck at its actual pitch) and keep the breakdown for display.

import { type Camelot, parseKey, transpose, wheelDistance } from "./library";
import type { Track } from "./types";

export interface Reference {
  track: Track;
  /** Effective BPM: what the deck is actually playing at. */
  bpm: number | null;
  /** Effective key (shifted by pitch when keylock is off). */
  key: Camelot | null;
  /** Desired energy change: -0.15 drop, 0 hold, +0.15 build. */
  energyDelta: number;
  /** Tracks already played this session. */
  played: Set<number>;
}

export interface Scored {
  track: Track;
  total: number;
  key: number;
  tempo: number;
  vibe: number;
  energy: number;
  /** Candidate tempo after half/double-time matching, as a % change from the reference. */
  tempoChange: number | null;
}

const W = { key: 0.35, tempo: 0.3, vibe: 0.25, energy: 0.1 };
export const TEMPO_TOLERANCE = 0.06;

export function keyScore(a: Camelot | null, b: Camelot | null): number {
  if (!a || !b) return 0.3;
  const d = wheelDistance(a, b);
  if (a.minor === b.minor) {
    if (d === 0) return 1;
    if (d === 1) return 0.9;
    if (d === 2) return 0.45; // energy boost (+2)
    if (d === 5) return 0.35; // up or down a semitone (7 steps round the wheel)
    return 0;
  }
  // Relative major/minor shares the number.
  if (d === 0) return 0.85;
  if (d === 1) return 0.4; // diagonal move
  return 0;
}

/** Percentage tempo change to reach `cand` from `ref`, allowing half/double time. */
export function tempoChange(ref: number, cand: number): number {
  let best = Infinity;
  for (const c of [cand, cand * 2, cand / 2]) {
    const ch = c / ref - 1;
    if (Math.abs(ch) < Math.abs(best)) best = ch;
  }
  return best;
}

function cosine(a: Float32Array, b: Float32Array): number {
  let s = 0;
  for (let i = 0; i < a.length; i++) s += a[i] * b[i];
  return s;
}

export function score(ref: Reference, cand: Track): Scored {
  const ck = parseKey(cand.key);
  const key = keyScore(ref.key, ck);
  let tempo = 0.3;
  let ch: number | null = null;
  if (ref.bpm && cand.bpm) {
    ch = tempoChange(ref.bpm, cand.bpm);
    tempo = Math.max(0, 1 - Math.abs(ch) / TEMPO_TOLERANCE);
  }
  // Unit vectors; map cosine -1..1 to 0..1. Missing embeddings score neutral.
  const vibe = ref.track.vibe && cand.vibe ? (cosine(ref.track.vibe, cand.vibe) + 1) / 2 : 0.5;
  const energy = Math.max(0, 1 - Math.abs(cand.energy - ref.track.energy - ref.energyDelta) * 3);
  let total = W.key * key + W.tempo * tempo + W.vibe * vibe + W.energy * energy;
  if (tempo === 0) total *= 0.3; // unmixable without a big pitch change
  if (ref.played.has(cand.id)) total *= 0.2;
  return { track: cand, total, key, tempo, vibe, energy, tempoChange: ch };
}

export function suggest(ref: Reference, tracks: Track[], n = 60): Scored[] {
  const out: Scored[] = [];
  let floor = -1;
  for (const t of tracks) {
    if (t.id === ref.track.id) continue;
    const s = score(ref, t);
    if (out.length >= n && s.total <= floor) continue;
    out.push(s);
    if (out.length > n * 2) {
      out.sort((a, b) => b.total - a.total);
      out.length = n;
      floor = out[n - 1].total;
    }
  }
  out.sort((a, b) => b.total - a.total);
  return out.slice(0, n);
}

/** Effective key of a deck: pitching by `speed` without keylock moves the key. */
export function effectiveKey(key: string | null, speed: number): Camelot | null {
  const k = parseKey(key);
  if (!k) return null;
  return transpose(k, Math.round(12 * Math.log2(speed)));
}

/**
 * Edges for the graph: each track's best few compatible neighbours. Candidates come
 * from the compatible key buckets (same, ±1, relative), and only the `nearest` tracks
 * by BPM in each bucket are scored, so the cost grows linearly with library size.
 * Scores here are plain numbers (no per-pair objects); this runs over ~10k tracks in
 * the webview's JS engine, where allocation is the main cost.
 */
export function compatibilityEdges(tracks: Track[], perTrack = 4, nearest = 40): [number, number][] {
  const keys = tracks.map((t) => parseKey(t.key));
  const buckets = new Map<string, number[]>();
  tracks.forEach((t, i) => {
    const k = keys[i];
    if (!k || !t.bpm) return;
    const id = `${k.n}${k.minor}`;
    const b = buckets.get(id) ?? [];
    b.push(i);
    buckets.set(id, b);
  });
  for (const b of buckets.values()) b.sort((x, y) => tracks[x].bpm! - tracks[y].bpm!);

  const seen = new Set<number>();
  const edges: [number, number][] = [];
  const bestI = new Int32Array(perTrack);
  const bestS = new Float64Array(perTrack);
  tracks.forEach((t, me) => {
    const k = keys[me];
    const bpm = t.bpm;
    if (!k || !bpm) return;
    bestI.fill(-1);
    bestS.fill(-1);
    for (const kk of [k, transpose(k, 7), transpose(k, -7), { n: k.n, minor: !k.minor }]) {
      const b = buckets.get(`${kk.n}${kk.minor}`);
      if (!b) continue;
      const ks = keyScore(k, kk);
      // Walk outwards from the closest BPM in this bucket.
      let hi = lowerBound(tracks, b, bpm);
      let lo = hi - 1;
      for (let taken = 0; taken < nearest && (lo >= 0 || hi < b.length); taken++) {
        const dl = lo >= 0 ? bpm - tracks[b[lo]].bpm! : Infinity;
        const dh = hi < b.length ? tracks[b[hi]].bpm! - bpm : Infinity;
        const j = dl <= dh ? b[lo--] : b[hi++];
        if (Math.min(dl, dh) > bpm * TEMPO_TOLERANCE) break;
        if (j === me) continue;
        const c = tracks[j];
        const tempo = 1 - Math.abs(c.bpm! / bpm - 1) / TEMPO_TOLERANCE;
        let vibe = 0.5;
        if (t.vibe && c.vibe) {
          let dot = 0;
          for (let d = 0; d < t.vibe.length; d++) dot += t.vibe[d] * c.vibe[d];
          vibe = (dot + 1) / 2;
        }
        const sc = W.key * ks + W.tempo * tempo + W.vibe * vibe;
        // Insert into the small top-k list.
        let pos = perTrack;
        while (pos > 0 && sc > bestS[pos - 1]) pos--;
        if (pos < perTrack) {
          bestS.copyWithin(pos + 1, pos, perTrack - 1);
          bestI.copyWithin(pos + 1, pos, perTrack - 1);
          bestS[pos] = sc;
          bestI[pos] = j;
        }
      }
    }
    for (const j of bestI) {
      if (j < 0) continue;
      const id = me < j ? me * tracks.length + j : j * tracks.length + me;
      if (seen.has(id)) continue;
      seen.add(id);
      edges.push([me, j]);
    }
  });
  return edges;
}

function lowerBound(tracks: Track[], b: number[], bpm: number) {
  let lo = 0;
  let hi = b.length;
  while (lo < hi) {
    const m = (lo + hi) >> 1;
    if (tracks[b[m]].bpm! < bpm) lo = m + 1;
    else hi = m;
  }
  return lo;
}
