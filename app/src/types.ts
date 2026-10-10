export interface GridDto {
  times: number[];
  positions: number[];
  source: string;
}

export interface LoadedTrack {
  title: string;
  artist: string;
  duration: number;
  sample_rate: number;
  grid: GridDto | null;
  waveform_bins_per_second: number;
  waveform_detail_per_second: number;
}

export interface DeckState {
  position: number;
  duration: number;
  playing: boolean;
  speed: number;
  rate: number;
  bpm: number | null;
  sync: boolean;
  loop_in: number | null;
  loop_out: number | null;
  loop_active: boolean;
  cue_point: number;
  hotcues: (number | null)[];
  volume: number;
  pfl: boolean;
  generation: number;
}

export interface AudioStats {
  callbacks_per_second: number;
  slow_callbacks: number;
  xruns: number;
  max_callback_ms: number;
  budget_ms: number;
  block_frames: number;
  sample_rate: number;
}

export interface EngineState {
  decks: DeckState[];
  crossfader: number;
  quantize: boolean;
  audio: AudioStats;
}

export interface OutputInfo {
  device: string;
  sample_rate: number;
  channels: number;
  buffer_frames: number | null;
  sample_format: string;
  null: boolean;
}

/** A library track from `scan_library` (one row of earshot.db). */
export interface LibTrack {
  id: number;
  path: string;
  title: string;
  artist: string;
  duration: number | null;
  /** From analysis; null until analysed. */
  bpm: number | null;
  tag_bpm: number | null;
  key: string | null;
  grid_source: string | null;
  drifts: boolean;
  /** Stand-in energy; only its order means anything. */
  energy: number | null;
}

export interface Subdir {
  name: string;
  path: string;
  has_children: boolean;
}

export type AnalysisState = "queued" | "running" | "done" | "error";

export interface AnalysisEvent {
  track_id: number;
  state: AnalysisState;
  /** 0..1 while running. */
  progress: number | null;
  stage: string | null;
  bpm: number | null;
  grid_source: string | null;
  error: string | null;
  /** After a static analysis: the tempo seems to drift (try dynamic). */
  drifts: boolean | null;
}

export interface DeckGridEvent {
  deck: number;
  grid: GridDto | null;
}

export type TrackSource =
  | { kind: "demo"; bpm: number; end_bpm?: number; key: string; seed: number; seconds?: number; title?: string }
  | { kind: "track"; id: number };

export interface Track {
  id: number;
  title: string;
  artist: string;
  bpm: number | null;
  /** Camelot notation, e.g. "8A". */
  key: string | null;
  /** 0..1 */
  energy: number;
  /** Stand-in for a CLAP embedding (unit vector), or null when not analysed. */
  vibe: Float32Array | null;
  /** Vibe cluster, for colouring the synthetic library. */
  cluster: number;
  source: TrackSource;
  /** Library tracks only. */
  path?: string;
  /** Raw stand-in energy from the file, before ranking into `energy`. */
  energyRaw?: number | null;
  duration?: number | null;
  /** A beatgrid from analysis, a hand fix or a tempo-lock sidecar exists. */
  analysed?: boolean;
  analysis?: AnalysisState | null;
  analysisProgress?: number | null;
  analysisStage?: string | null;
  analysisError?: string | null;
  /** A static analysis found the tempo seems to drift. */
  drifts?: boolean;
  /** The grid follows the track's drift (dynamic analysis). */
  dynamic?: boolean;
}

export interface MidiEvent {
  port: string;
  bytes: number[];
  mapped: string | null;
}

export interface UiControl {
  group: string;
  key: string;
  value: number;
}
