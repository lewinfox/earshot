# What's left after batches 1 and 2

Batches 1 and 2 of the rebuild (review steps 0–6) are merged as of `b0ed7bb`.
This list collects what the agents left undone, for the UI rebuild (step 7) and
after.

## Step 7: UI rebuild
Done (see the README). One open idea, from the user's feature notes:
- [ ] Prepare layout: the left icon stacks pick what we're browsing (sources and
  collections) and the tabs pick the tool, so browsing is the same in both modes.
  Tempo lock and Flow sit right of the library now; whether Cues & Grid (the
  player's controls, one wide row) should too is undecided.

## Library and curation
- [x] Save play history to the `plays` table (and `sessions`: one per run of the
  app). "Played this session" still means this run, so nothing is read back yet.
- [ ] Save the last-open Flow and its camera position in the DB (still in eframe
  storage).
- [x] Undo/redo (`core::edit::History`): metadata, grid edits, collections, Flow
  edits; creating, duplicating and deleting collections (phase C); removing tracks
  from the library.
- [x] Metadata editing UI: "Edit info…" in the track menu (one track or many; mixed
  fields are left alone unless edited). Beside each text field, "file" or "edited",
  with Revert to go back to the file's value. Undoable.
- [x] Duplicates view (All tracks > Library > Duplicates), with "Show in file
  manager" and "Remove from library" (undoable; files are never deleted).
- [ ] Fill `tracks.key_pc` / `key_minor` so key search in SQL doesn't need text
  matching.
- [ ] `TrackId` is an `i64` alias; consider a newtype.

## Analysis
- [x] Check at worker start-up that its grid analyser version (sent in `hello`)
  matches the Rust registry's. On a mismatch the worker is refused until a restart
  and the top bar says so.
- [x] UI for "re-analyse stale tracks" (`Analyser::queue_stale`): Settings >
  Analysis, with the count.
- [x] UI for the background worker cap (`analysis.background_workers` setting,
  default 4): Settings > Analysis, applied at once. How the cores are split between
  the workers is still set at startup.
- [ ] Removing a track should cancel its queued jobs (they fail harmlessly now).
- [ ] CLAP embeddings: load once into one contiguous in-memory block and search
  there. Registry fields `AudioNeeds` and `depends_on` are ready for it.

## Performance and control
- [x] Once (2026-10-13): Prepare's player ignored input while analysis ran. Most
  likely a load that never finished, which leaves a player on "loading" (it ignores
  its controls then); a crashed load now fails instead (`53164ac`).
- [x] Resync `DeckSession` if the engine event ring overflows (a hot cue change
  could be missed): from a snapshot, when `Stats::dropped_events` grows.
- [x] Load a user keymap from disk: `keymap.txt` in the data folder, shown in
  Settings > Keymap with its errors; the built-in one otherwise.
- [x] Two loads onto one player in flight: only the newest lands (load tickets).
- [x] Show the play history: All tracks > Library > History lists the sessions, each
  one's tracks in play order (with "Save as playlist"); Plays and Last played columns
  (and `plays>=3`, `played<7` in searches).
- [ ] LED feedback to controllers (`ControlSpec` getters + `EngineEvent`s).
- [ ] Effects on the `Processor` chain; slip, reverse, censor via
  `Transport::jump`.
