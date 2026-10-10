-- Library folders, the tracks found in them, and what analysis has worked out.
-- A subset of the schema sketched in docs/design.md ("Database").

CREATE TABLE library_folders (
  id        INTEGER PRIMARY KEY,
  path      TEXT NOT NULL UNIQUE,
  added_at  TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE tracks (
  id          INTEGER PRIMARY KEY,
  path        TEXT NOT NULL UNIQUE,
  size        INTEGER NOT NULL,
  mtime       INTEGER NOT NULL,          -- seconds since the epoch; tags are re-read when size or mtime change
  duration_s  REAL,
  title       TEXT,
  artist      TEXT,
  tag_bpm     REAL,
  tag_key     TEXT,
  added_at    TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE analysis (
  track_id     INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
  bpm          REAL,
  bpm_min      REAL,
  bpm_max      REAL,
  detector     TEXT,
  analysed_at  TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- `beats` is the binary format in engine/src/beatgrid.rs. `source` is 'analysis',
-- 'user' (hand-edited) or 'tempo-lock sidecar'.
CREATE TABLE beatgrids (
  track_id    INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
  beats       BLOB NOT NULL,
  source      TEXT NOT NULL,
  updated_at  TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- 4 bytes per bin: peak, low, mid, high (engine/src/waveform.rs).
CREATE TABLE waveforms (
  track_id    INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
  data        BLOB NOT NULL,
  bins_per_s  REAL NOT NULL,
  version     INTEGER NOT NULL
);
