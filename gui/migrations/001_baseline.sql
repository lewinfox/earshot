-- The baseline schema (docs/review/data-model.md §3). Every table is STRICT.
-- Times are seconds since the epoch unless the column name says otherwise.
-- Only the Store thread (gui/src/core/store) writes to this database.

-- ---- track identity ---------------------------------------------------------------

-- A track is its id, not its path: files can move, and a track can have had several.
CREATE TABLE tracks (
  id              INTEGER PRIMARY KEY,
  added_at        INTEGER NOT NULL,
  duration_s      REAL,
  sample_rate     INTEGER,
  derived_from    INTEGER REFERENCES tracks(id) ON DELETE SET NULL,  -- e.g. a tempo-lock render
  derivation      TEXT,
  -- The grid in use. Analysis and user edits each add a beatgrids row; only the
  -- Store switches this, in the same transaction that sets `bpm`.
  active_grid_id  INTEGER REFERENCES beatgrids(id) ON DELETE SET NULL,
  -- Effective values, copied from the active grid (and, later, key analysis).
  bpm             REAL,
  key_pc          INTEGER,            -- pitch class 0-11 (not filled in yet)
  key_minor       INTEGER,
  rating          INTEGER,
  color           INTEGER,
  deleted_at      INTEGER
) STRICT;

-- Where a track's audio is (or was). `path` is the OS path's raw bytes.
-- `content_hash` is the SHA-256 of the audio, not the tags (core/hash.rs): from the
-- file's TXXX:AUDIO_SHA256 frame when it has one, else worked out in the background.
CREATE TABLE files (
  id             INTEGER PRIMARY KEY,
  track_id       INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  path           BLOB NOT NULL UNIQUE,
  size           INTEGER NOT NULL,
  mtime_ns       INTEGER NOT NULL,
  content_hash   BLOB,
  missing_since  INTEGER
) STRICT;
CREATE INDEX files_track ON files(track_id);
CREATE INDEX files_hash ON files(content_hash);

-- ---- metadata ---------------------------------------------------------------------

-- One value per field per source. A rescan only ever writes source 'tag', so what the
-- user typed ('user') or another library said ('import:<name>') survives it.
CREATE TABLE track_meta (
  track_id    INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  field       TEXT NOT NULL,           -- title, artist, album, genre, comment, bpm, key …
  source      TEXT NOT NULL CHECK (source IN ('tag', 'user') OR source LIKE 'import:%'),
  value       TEXT NOT NULL,
  updated_at  INTEGER NOT NULL,
  PRIMARY KEY (track_id, field, source)
) STRICT;

-- The value that wins for each field: user > import > tag.
CREATE VIEW track_meta_resolved AS
SELECT track_id, field, value, source FROM (
  SELECT track_id, field, value, source,
         row_number() OVER (
           PARTITION BY track_id, field
           ORDER BY CASE WHEN source = 'user' THEN 0 WHEN source = 'tag' THEN 2 ELSE 1 END
         ) AS n
  FROM track_meta)
WHERE n = 1;

-- Full-text search over the resolved text fields; rowid is the track id. Kept up to
-- date by the Store whenever a track's metadata changes.
CREATE VIRTUAL TABLE track_search USING fts5(
  title, artist, album, genre, comment,
  content = '', contentless_delete = 1, tokenize = 'unicode61 remove_diacritics 2'
);

-- ---- analysis ---------------------------------------------------------------------

CREATE TABLE analysis_runs (
  id           INTEGER PRIMARY KEY,
  track_id     INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  analyser     TEXT NOT NULL,          -- e.g. 'static', 'beat_this'
  version      TEXT,
  mode         TEXT,                   -- 'static' | 'dynamic'
  started_at   INTEGER NOT NULL,
  finished_at  INTEGER,
  error        TEXT
) STRICT;
CREATE INDEX analysis_runs_track ON analysis_runs(track_id);

-- The latest result of each analyser (gui/src/core/analysis/registry.rs) for each
-- track. Stale when `version` is below the analyser's, or `content_hash` isn't the
-- audio the track has now (NULL on either side: not known, so not stale). A failed
-- run is a row with `error` set.
CREATE TABLE analysis_results (
  track_id      INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  analyser      TEXT NOT NULL,           -- waveform, loudness, energy, grid
  version       INTEGER NOT NULL,
  content_hash  BLOB,                    -- files.content_hash of the audio analysed
  run_id        INTEGER REFERENCES analysis_runs(id) ON DELETE SET NULL,
  error         TEXT,
  analysed_at   INTEGER NOT NULL,
  PRIMARY KEY (track_id, analyser)
) STRICT;

-- Derived numbers: energy, lufs, peak, bpm_min, bpm_max, drifts (0/1) …
CREATE TABLE track_features (
  track_id  INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  name      TEXT NOT NULL,
  value     REAL NOT NULL,
  run_id    INTEGER REFERENCES analysis_runs(id) ON DELETE SET NULL,
  PRIMARY KEY (track_id, name)
) STRICT;

-- Every grid a track has had, as its model (audio/src/beatgrid.rs `GridModel`):
--   constant: a beat at `anchor_s`, then one every 60 / `bpm` s either way. That beat
--             is beat `anchor_pos` (1 = the downbeat) of a bar of `meter` beats.
--   markers:  every beat, in `beats` (beatgrid.rs's binary format); `bpm` is their
--             average, rounded to 0.01.
-- Times are seconds of our decoded audio. Export to Rekordbox or Mixxx will need the
-- grid's timebase here too (decoder, offset from the file's first sample).
CREATE TABLE beatgrids (
  id          INTEGER PRIMARY KEY,
  track_id    INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  source      TEXT NOT NULL CHECK (source IN ('analysis', 'analysis-dynamic', 'user', 'sidecar', 'import')),
  run_id      INTEGER REFERENCES analysis_runs(id) ON DELETE SET NULL,
  kind        TEXT NOT NULL CHECK (kind IN ('constant', 'markers')),
  bpm         REAL NOT NULL,
  anchor_s    REAL,
  anchor_pos  INTEGER,
  meter       INTEGER,
  beats       BLOB,
  created_at  INTEGER NOT NULL,
  CHECK (CASE kind
           WHEN 'constant' THEN anchor_s IS NOT NULL AND anchor_pos IS NOT NULL
                                AND meter IS NOT NULL AND beats IS NULL
           ELSE beats IS NOT NULL AND anchor_s IS NULL AND anchor_pos IS NULL AND meter IS NULL
         END)
) STRICT;
CREATE INDEX beatgrids_track ON beatgrids(track_id);

-- Cached binary data: 'waveform' (engine waveform::summarise; params 'bins_per_s=…'),
-- 'waveform_preview' (waveform::preview). Stale when `format_version` is old.
CREATE TABLE blobs (
  track_id        INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  kind            TEXT NOT NULL,
  format_version  INTEGER NOT NULL,
  params          TEXT,
  data            BLOB NOT NULL,
  PRIMARY KEY (track_id, kind)
) STRICT;

CREATE TABLE embeddings (
  track_id  INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  model     TEXT NOT NULL,
  dim       INTEGER NOT NULL,
  vector    BLOB NOT NULL,
  PRIMARY KEY (track_id, model)
) STRICT;

-- ---- performance ------------------------------------------------------------------

CREATE TABLE cues (
  id        INTEGER PRIMARY KEY,
  track_id  INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  kind      INTEGER NOT NULL,          -- hot cue, memory cue, loop …
  slot      INTEGER,
  start_s   REAL NOT NULL,
  end_s     REAL,
  label     TEXT,
  color     INTEGER,
  source    TEXT NOT NULL DEFAULT 'user',
  UNIQUE (track_id, kind, slot)
) STRICT;

-- ---- curation ---------------------------------------------------------------------

CREATE TABLE tags (
  id        INTEGER PRIMARY KEY,
  category  TEXT NOT NULL DEFAULT '',
  name      TEXT NOT NULL,
  UNIQUE (category, name)
) STRICT;

CREATE TABLE track_tags (
  track_id    INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  tag_id      INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
  source      TEXT NOT NULL DEFAULT 'user',
  confidence  REAL,
  PRIMARY KEY (track_id, tag_id)
) STRICT;

-- Folders, crates, playlists, smart playlists and Flows, in one tree. What a
-- collection holds depends on its kind:
--   crate:    crate_tracks (a set: each track at most once, no order)
--   playlist: playlist_entries (an ordered list; a track may repeat). A playlist
--             exported from a Flow is a snapshot of one path through it and
--             remembers which Flow in `source_flow_id`.
--   smart:    `query`
--   flow:     flow_nodes + flow_edges (a DAG; a track may appear on several nodes)
CREATE TABLE collections (
  id              INTEGER PRIMARY KEY,
  parent_id       INTEGER REFERENCES collections(id) ON DELETE CASCADE,  -- a folder; NULL: top level
  kind            TEXT NOT NULL CHECK (kind IN ('folder', 'crate', 'playlist', 'smart', 'flow')),
  name            TEXT NOT NULL,
  query           TEXT,
  sort_order      INTEGER,
  view_state      TEXT,
  source_flow_id  INTEGER REFERENCES collections(id) ON DELETE SET NULL,
  created_at      INTEGER NOT NULL,
  updated_at      INTEGER NOT NULL
) STRICT;
CREATE INDEX collections_parent ON collections(parent_id);

CREATE TABLE crate_tracks (
  collection_id  INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
  track_id       INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  added_at       INTEGER NOT NULL,
  PRIMARY KEY (collection_id, track_id)
) STRICT;
CREATE INDEX crate_tracks_track ON crate_tracks(track_id);

CREATE TABLE playlist_entries (
  collection_id  INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
  position       INTEGER NOT NULL,
  track_id       INTEGER NOT NULL REFERENCES tracks(id) ON DELETE RESTRICT,
  PRIMARY KEY (collection_id, position)
) STRICT;
CREATE INDEX playlist_entries_track ON playlist_entries(track_id);

-- A node's id is the card id the Flow canvas gave it, unique within its Flow.
CREATE TABLE flow_nodes (
  collection_id  INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
  id             INTEGER NOT NULL,
  track_id       INTEGER NOT NULL REFERENCES tracks(id) ON DELETE RESTRICT,
  x              REAL NOT NULL,
  y              REAL NOT NULL,
  PRIMARY KEY (collection_id, id)
) STRICT;
CREATE INDEX flow_nodes_track ON flow_nodes(track_id);

-- `to_node` follows `from_node`. No cycles: core/flows.rs checks before the Store saves.
CREATE TABLE flow_edges (
  collection_id  INTEGER NOT NULL,
  from_node      INTEGER NOT NULL,
  to_node        INTEGER NOT NULL,
  PRIMARY KEY (collection_id, from_node, to_node),
  FOREIGN KEY (collection_id, from_node) REFERENCES flow_nodes(collection_id, id) ON DELETE CASCADE,
  FOREIGN KEY (collection_id, to_node) REFERENCES flow_nodes(collection_id, id) ON DELETE CASCADE
) STRICT;

-- ---- history and sync -------------------------------------------------------------

CREATE TABLE sessions (
  id          INTEGER PRIMARY KEY,
  started_at  INTEGER NOT NULL,
  ended_at    INTEGER
) STRICT;

CREATE TABLE plays (
  session_id  INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  track_id    INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  deck        INTEGER NOT NULL,
  started_at  INTEGER NOT NULL,
  ended_at    INTEGER,
  counted     INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE INDEX plays_track ON plays(track_id);

-- The same track in another library (cratelib, Rekordbox …).
CREATE TABLE external_links (
  track_id     INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  library      TEXT NOT NULL,
  external_id  TEXT NOT NULL,
  synced_at    INTEGER,
  etag         TEXT,
  PRIMARY KEY (library, external_id)
) STRICT;
CREATE INDEX external_links_track ON external_links(track_id);

-- Background work that should survive a restart. (Audio hashing doesn't need it:
-- its queue is `files.content_hash IS NULL`.) Analysis jobs: `kind` is
-- 'analyse:<analyser>', one row per track and analyser while it's queued (deleted when
-- done). `want` is missing | stale | force; `params` the grid mode.
CREATE TABLE jobs (
  id           INTEGER PRIMARY KEY,
  kind         TEXT NOT NULL,
  track_id     INTEGER REFERENCES tracks(id) ON DELETE CASCADE,
  status       TEXT NOT NULL DEFAULT 'queued' CHECK (status IN ('queued', 'running', 'done', 'error')),
  error        TEXT,
  created_at   INTEGER NOT NULL,
  finished_at  INTEGER,
  want         TEXT,
  params       TEXT
) STRICT;
CREATE UNIQUE INDEX jobs_track_kind ON jobs(track_id, kind);

-- ---- settings ---------------------------------------------------------------------

-- Folders scanned for music; `path` is the OS path's raw bytes.
CREATE TABLE library_roots (
  id        INTEGER PRIMARY KEY,
  path      BLOB NOT NULL UNIQUE,
  added_at  INTEGER NOT NULL
) STRICT;

-- Settings the analysis reads: 'analysis.background_workers' (how many background
-- worker processes may run at once).
CREATE TABLE settings (
  key    TEXT PRIMARY KEY,
  value  TEXT NOT NULL
) STRICT;
