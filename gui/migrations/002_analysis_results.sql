-- Analysis results per analyser (gui/src/core/analysis/registry.rs), and the analysis
-- job queue in `jobs`.

-- The latest result of each analyser for each track. Stale when `version` is below
-- the analyser's, or `content_hash` isn't the audio the track has now (NULL on either
-- side: not known, so not stale). A failed run is a row with `error` set.
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

-- Results from before this table. Grids get version 0: which analyser version made
-- them isn't known, so they count as stale (but are kept, and not redone on load).
INSERT INTO analysis_results(track_id, analyser, version, content_hash, analysed_at)
SELECT t.id, 'grid', 0,
       (SELECT f.content_hash FROM files f WHERE f.track_id = t.id AND f.missing_since IS NULL ORDER BY f.id LIMIT 1),
       g.created_at
FROM tracks t JOIN beatgrids g ON g.id = t.active_grid_id
WHERE g.source IN ('analysis', 'analysis-dynamic');

INSERT INTO analysis_results(track_id, analyser, version, content_hash, analysed_at)
SELECT b.track_id, 'waveform', b.format_version,
       (SELECT f.content_hash FROM files f WHERE f.track_id = b.track_id AND f.missing_since IS NULL ORDER BY f.id LIMIT 1),
       0
FROM blobs b WHERE b.kind = 'waveform';

INSERT INTO analysis_results(track_id, analyser, version, content_hash, analysed_at)
SELECT DISTINCT tf.track_id, CASE tf.name WHEN 'lufs' THEN 'loudness' ELSE 'energy' END, 1,
       (SELECT f.content_hash FROM files f WHERE f.track_id = tf.track_id AND f.missing_since IS NULL ORDER BY f.id LIMIT 1),
       0
FROM track_features tf WHERE tf.name IN ('lufs', 'energy');

-- Analysis jobs: `kind` is 'analyse:<analyser>', one row per track and analyser while
-- it's queued (deleted when done). `want` is missing | stale | force; `params` the
-- grid mode.
ALTER TABLE jobs ADD COLUMN want TEXT;
ALTER TABLE jobs ADD COLUMN params TEXT;
CREATE UNIQUE INDEX jobs_track_kind ON jobs(track_id, kind);

-- Settings the analysis reads: 'analysis.background_workers' (how many background
-- worker processes may run at once).
