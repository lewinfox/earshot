-- Static (one steady tempo) or dynamic (follows drift) analysis, and whether a static
-- analysis thought the tempo drifts (so the UI can suggest dynamic).
ALTER TABLE analysis ADD COLUMN mode TEXT;
ALTER TABLE analysis ADD COLUMN drifts INTEGER;
