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

export interface ScannedTrack {
  path: string;
  title: string;
  artist: string;
  bpm: number | null;
  key: string | null;
  duration: number | null;
  has_beats_sidecar: boolean;
}

export type TrackSource =
  | { kind: "demo"; bpm: number; end_bpm?: number; key: string; seed: number; seconds?: number; title?: string }
  | { kind: "file"; path: string };

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
