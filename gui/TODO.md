# What's left

## 1. Check it on a real screen

The first pass was only checked off-screen (kittest renders and frame timing).

- [x] Run it in a window: 59–60 fps, also while analysing several tracks.
- [x] Import a folder through *Menu → Import → Folder…* (rfd dialog).
- [ ] DDJ-400 over MIDI: every mapped control, and that the jog wheels feel right.
      (Waiting on the controller.)
- [x] Keyboard shortcuts, including held buttons (cue, nudge) and Shift + hot cue to clear.
- [x] Panel sizes, zoom and UI size survive a restart.

## 2. Suggestions

- [x] Scorer in `core/suggest.rs` (key 0.35, tempo 0.3, vibe 0.25, energy 0.1), half and
      double tempo, per-part breakdown, unit tests. Vibe is a neutral 0.5 until there
      are sound-alike embeddings.
- [x] Reference track: the deck that started playing most recently, at its actual
      tempo and pitch; otherwise the track selected in the library.
- [x] "Played this session": counts after 30 s of play; dimmed in both lists and
      scored lower.
- [x] Suggestions pane on the right: Drop / Hold / Build, ranked rows with score bars
      (hover for numbers), double-click, drag or right-click to load. Tab (or the
      controller's browse button) switches the keyboard between library and suggestions.
- [ ] Try it in a real set: are the rankings sensible?

## 3. Flow canvas

- [x] Tracks | Flow tabs above the library list.
- [x] Canvas on egui's `Scene`: drag the background to pan, wheel to zoom, drag cards
      to move them. Cards show art, title, artist, BPM, key,
      energy and time, with curved edges between them.
- [x] Add tracks with *Add track* (search), by dragging from the list, or with
      right-click > *Add to Flow* (works on a multi-selection).
- [x] "+" on each card opens nine suggestions with Drop / Hold / Build; picking one
      adds a connected card. Tracks already in the Flow are left out.
- [x] Play button previews the track in the headphones (stop square while playing,
      progress along the card's bottom). Double-click or right-click loads to a deck.
- [x] Link cards by dragging from a "+" to another card's left port (or the other
      way); loops are refused. Right-click > *Unlink from previous card*.
- [x] Fit, Reset, Full screen (hides the decks and side panes).
- [x] Kept across restarts in eframe's storage.
- [x] Drag a card onto a deck to load it (the card goes back to where it was).
- [ ] Try it on a real screen: zoom feel, popups, linking.

## 4. Library graph (later)

Parked for now; we'll come back to it. The idea: an egui painter with a wheel layout
(Camelot) and a vibe layout (PCA), key colours, hover and select synced with the list.
Plain egui shapes should cope with ~2,000 points; check that.

## 5. Smaller gaps

- [x] Audio window (*Menu > Audio…* or click the audio pill): pick any output device and
      buffer size (restarts audio, decks stay loaded), and route master and headphones
      to any channel pair or single channel, live. Presets: Controller, Speakers only,
      Split cue. Saved between runs.
- [x] Preview player in the engine: headphone bus only. In the Flow, and right-click >
      *Preview in headphones* in the library and Suggestions.
- [ ] Try routing and preview on real hardware (laptop, then the DDJ-400).
- [ ] Separate devices for master and headphones, e.g. headphones to the laptop's
      jack and master to HDMI. A plugged-in controller adds its outputs to the choices
      and is the default when present.
- [x] Reconnect MIDI by clicking the MIDI pill.
- [x] Library control from MIDI: browse, switch between library and Suggestions, load.

## 6. Housekeeping

- [ ] Drop the `ordered-float` 5.0.0 pin in `Cargo.lock` when Rust is updated past 1.89.
