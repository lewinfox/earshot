# What's left after batches 1 and 2

Batches 1 and 2 of the rebuild (review steps 0–6) are merged as of `b0ed7bb`.
This list collects what the agents left undone, for the UI rebuild (step 7) and
after.

## Step 7: UI rebuild
- [ ] Recreate everything in [post-review-ui-changes.md](post-review-ui-changes.md).
- [ ] Theme module: one place for colours (no raw RGB in widgets).
- [ ] `Shell` with modes (Prepare, Perform, later Library), panels and modals.
  Layout saved per mode in `UiPrefs`.
- [ ] One `TrackTable` with configurable, sortable columns (`ColumnDef`).
- [ ] One `focused_pane`: keyboard, MIDI browse and *Selected* suggestions follow
  it, including the Flow.
- [ ] Multi-track drag payload (`DragPayload::Tracks { ids, origin }`). Drags carry
  one id today.
- [ ] On-demand repaint.
- [ ] Inject `Services` so the app-level kittest tests run in CI.
- [ ] UI for up to 4 decks, crossfader side per deck.
- [ ] Reordering playlist rows.

## Library and curation
- [x] Save play history to the `plays` table (and `sessions`: one per run of the
  app). "Played this session" still means this run, so nothing is read back yet.
- [ ] Save the last-open Flow and its camera position in the DB (still in eframe
  storage).
- [x] Undo/redo (`core::edit::History`): metadata, grid edits, collections, Flow
  edits; creating, duplicating and deleting collections (phase C); removing tracks
  from the library.
- [ ] Metadata editing UI (the Store keeps user values with `source='user'`).
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
- [ ] Once (2026-10-13, not reproduced): Prepare's player seemed to ignore input while
  background analysis was running. If it comes back, suspect a lock the UI thread
  shares with analysis (player mutex, job queue mutex).
- [x] Resync `DeckSession` if the engine event ring overflows (a hot cue change
  could be missed): from a snapshot, when `Stats::dropped_events` grows.
- [x] Load a user keymap from disk: `keymap.txt` in the data folder, shown in
  Settings > Keymap with its errors; the built-in one otherwise.
- [x] Two loads onto one player in flight: only the newest lands (load tickets).
- [ ] Show the play history somewhere (it's saved, nothing reads it yet).
- [ ] LED feedback to controllers (`ControlSpec` getters + `EngineEvent`s).
- [ ] Effects on the `Processor` chain; slip, reverse, censor via
  `Transport::jump`.
