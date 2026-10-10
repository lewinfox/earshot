-- Stand-in energy per track (engine/src/energy.rs); the UI ranks tracks by it.
ALTER TABLE tracks ADD COLUMN energy REAL;
