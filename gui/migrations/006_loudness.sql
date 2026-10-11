-- Integrated loudness (LUFS) and sample peak per track, for auto-gain
-- (engine/src/loudness.rs). Measured when a track is analysed or loaded.
ALTER TABLE tracks ADD COLUMN loudness REAL;
ALTER TABLE tracks ADD COLUMN peak REAL;
