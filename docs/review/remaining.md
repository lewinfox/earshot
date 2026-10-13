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
- [x] Save the last-open Flow and its camera position in the DB: the open Flow is
  the `flow.open` setting, each Flow's camera its own `collections.view_state`
  (sent back with the Flow in `StoreEvent::FlowLoaded`). UiPrefs holds neither.
- [x] Undo/redo (`core::edit::History`): metadata, grid edits, collections, Flow
  edits; creating, duplicating and deleting collections (phase C); removing tracks
  from the library.
- [ ] Metadata editing UI (the Store keeps user values with `source='user'`).
- [x] Duplicates view (All tracks > Library > Duplicates), with "Show in file
  manager" and "Remove from library" (undoable; files are never deleted).
- [x] Fill `tracks.key_pc` / `key_minor` (from the resolved key, whenever it
  changes; filled on opening for older databases). `key:` in SQL uses them.
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
- [x] Removing a track cancels its analysis (`Analyser::forget`): waiting jobs
  dropped, a running one cancelled (not a failure), saved queue rows deleted.
- [x] CLAP embeddings: one contiguous, normalised in-memory block
  (`core::embeddings`), loaded when the Store opens and kept in step by the writer
  (`Store::embeddings`). 5,000 × 512: 0.6 ms a search, against ~10 ms reading
  them from the database each time.
- [ ] The CLAP analyser itself (a worker analyser at 48 kHz: `AudioNeeds`), saving
  with `StoreCmd::SaveEmbedding`, and a Vibe suggestion feature searching the block.

## Performance and control
- [x] Once (2026-10-13): Prepare's player ignored input while analysis ran. Most
  likely a load that never finished, which leaves a player on "loading" (it ignores
  its controls then); a crashed load now fails instead (`53164ac`).
- [x] Resync `DeckSession` if the engine event ring overflows (a hot cue change
  could be missed): from a snapshot, when `Stats::dropped_events` grows.
- [x] Load a user keymap from disk: `keymap.txt` in the data folder, shown in
  Settings > Keymap with its errors; the built-in one otherwise.
- [x] Two loads onto one player in flight: only the newest lands (load tickets).
- [ ] Show the play history somewhere (it's saved, nothing reads it yet).
- [ ] LED feedback to controllers (`ControlSpec` getters + `EngineEvent`s).
- [ ] Effects on the `Processor` chain; slip, reverse, censor via
  `Transport::jump`.
