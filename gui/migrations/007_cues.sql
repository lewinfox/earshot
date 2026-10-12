-- Saved cues per track: hot cues (slot = pad 0-7) and memory cues and loops
-- (slot NULL). `end_s` is set for a loop.
CREATE TABLE cues (
  id        INTEGER PRIMARY KEY,
  track_id  INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  kind      TEXT NOT NULL CHECK (kind IN ('hot', 'memory')),
  slot      INTEGER,
  start_s   REAL NOT NULL,
  end_s     REAL
);
CREATE INDEX cues_track ON cues(track_id);
