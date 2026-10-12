# UI changes made after the review

The review (`92fd397`, line references to `8bb7c31`) was written before these
changes. They came in with `3c444db`. The rebuild steps port the data and engine
parts (saved cues, the Prepare player, analysis priority). The UI rebuild (step 7)
must recreate everything below. Tick each one off once it works in the new UI.

## Prepare / Perform modes
- [x] Mode picker (dropdown) in the top bar. The choice is remembered across runs.
  - **Prepare:** one player across the top, for setting cues, fixing grids and
    building playlists.
  - **Perform:** two decks and the mixer.
- [x] Prepare has its own player (Mixxx's preview deck group), playing straight to
  the main output.
- [x] Prepare layout, all in one row: title and overview, full-width waveform, then
  CUE / play and the BPM ring, hot cues, loops, beat jumps, the grid editor and the
  memory cue list.
- [x] Prepare's player shows no deck number.
- [x] Double-click in the library loads into Prepare's player in Prepare mode
  (in Perform: a deck that isn't playing, preferring A).

## Prepare keys (Rekordbox's)
- [x] ← / → beat jump.
- [x] Shift+← / → nudge the grid by 10 ms.
- [x] C sets the cue point at the playhead (snapped when quantising).
- [x] M saves the cue point, or the loop while looping, to memory.
- [x] 6 / 7 / 8 / 9 loop 1 / 2 / 4 / 8 bars. The same key again ends the loop.
- [x] Space plays and pauses.
- [x] ↑ / ↓ browse (keeps repeating while held), Tab switches pane, Enter loads.

## Cues
- [x] Hot cues on every player are saved per track and restored on load. They are
  saved only once the engine shows the load they belong to.
- [x] Memory cues and memory loops, saved per track.
- [x] Memory list: "MEMORY" label, a "+ M" button, click an entry to go there (a cue
  becomes the cue point; a loop starts), × to forget it. When empty it shows the
  hint "C sets a cue, M saves it here". Now a cue pane on the right of Prepare's
  waveform (like Rekordbox's): one row per memory cue or loop with its colour, time,
  loop length and name. Double-click a row to name it (Enter saves, Esc cancels),
  click the colour to pick one of 8. Memory cues show on the waveforms as coloured
  triangles along the bottom.
- [x] Hot cue labels: right-click a pad → "Label…" / "Clear label". The name shows on
  the pad and beside the hot cue's marker on the waveform. Moving a hot cue keeps its
  name; clearing it drops the name.
- [x] Quantise button ("Q") on Prepare's player.

## Beat jump and loops
- [x] Beat jump step picker ("STEP n", click to pick 1–32 beats) replaces the ½ / ×2
  buttons.
- [x] Picking a different beat-loop size while looping resizes the loop.
- [x] The loop button always ends an active loop, whatever its size by now.

## Grid editor
- [x] "Anchor 1 here" is a hardware-style button drawn as a red bar over white
  (Rekordbox's "put the 1 here" mark).
- [x] "Nearest beat = 1" is removed.

## Tests
- [x] A Prepare-mode screenshot test (writes to `EARSHOT_SHOT`).
