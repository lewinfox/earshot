# App shell and performance UI

Part of the [October 2026 architecture review](README.md). Line references are to `8bb7c31`.

## 1. How it's built today

### State model

All app state hangs off one struct, `EarshotApp` (`gui/src/ui/app.rs:29-65`), which has about 25 fields. Four kinds of state sit side by side in it:

- Engine handles: `player`, `shared`, `_midi`.
- Domain state: `last_started`, `play_started`, `analysis`, `preview`.
- View state: `pane`, `show_keys`, `half_window`, `held_keys`.
- Async plumbing: `picked`, `art_loader`, `rx`.

The child UI structs (`LibraryUi`, `SuggestUi`, `FlowUi`) also mix model and view, and most of their fields are `pub`. The app reaches in and changes them directly:

- Flow: `self.flow.open/full/flow/name` (`app.rs:296,316,342,1116,1185,1239`).
- Library: `self.library.folders/scanning/played/version/previews/art/search_focus` (`app.rs:373,450,694,761,819,1246`).
- Suggestions: `self.suggest.reference/computed_for/settling` (`app.rs:489-545`).

### How panes talk to each other

There are four action enums:

| Enum | Defined at | Used by |
|---|---|---|
| `deck::Action` | `deck.rs:28` | Decks, and reused by the mixer (`mixer.rs:18`) |
| `LibAction` | `library_ui.rs:55` | Library, suggestions and Flow |
| `FlowsAction` | `flows_ui.rs:18` | Flows tree |
| `AudioAction` | `audio_ui.rs:16` | Audio window |

On top of those, panes also talk through:

- `bool` return values: `suggest_ui` and `library.list` (`app.rs:1175,1199`).
- Direct writes to each other's fields.
- An egui temp-data side channel for art requests (`library_ui.rs:76-85`), drained at `app.rs:1254`.

Each enum has its own dispatch loop at the end of `update` (`app.rs:1219-1264`).

### Control bus

`core::controls::apply` (`controls.rs:165`) interprets `(group, key, f64)` strings at runtime. Three input paths use it:

- Keyboard (`app.rs:796`).
- Deck and mixer widgets, via `Action::Control`.
- MIDI. This applies controls directly on the MIDI thread (`midi.rs:207`) and forwards UI-owned controls back as `Event::UiControl`.

Several actions skip the bus entirely:

- Grid edits (`Action::Grid`).
- `Action::TempoRange`, even though a `rateRange` control exists.
- Loads and previews.
- The jump and loop size steppers.

## 2. Findings, most important first

### F1. There is no single command path; the control bus covers only part of the app

**Problem.**
- Toggle logic is written four times:
  - MIDI: `midi.rs:191-197`.
  - Keyboard: `app.rs:843-848`, using `controls::current` under the player lock.
  - Deck widgets: `deck.rs:808,1086,1098,1108`, using the UI snapshot.
  - Mixer: `mixer.rs:231`.
- Buttons don't agree on when they fire. Some act on press (`held(..)==Some(true)`, `deck.rs:807`); others act on release (`clicked()`, `deck.rs:1085`, `mixer.rs:227`).
- The key help text in `keys_help` (`app.rs:1340-1374`) is a second hand-written copy of the key map in `deck_key` (`app.rs:1290-1323`).

**Impact.** Every new feature (effects, slip, library bulk edit, MIDI mappings) adds yet another input path. MIDI mappings can only target what is on the bus, so there is no MIDI for grid edits, previews or the Flow.

**Recommendation.** One typed `Command` enum and one `dispatch` function. The `(group, key)` strings become a parsing layer at the edges (MIDI and keymap files) that produces typed commands.

### F2. Control definitions are split between core and UI

**Problem.**
- Mixxx control names and meanings are in `core/controls.rs`, but ranges, defaults and display formats are in `ui/mixer.rs:22-99` and `deck.rs:168-192`.
- State is read three different ways: `controls::value` (`controls.rs:324`), `controls::current` (`controls.rs:309`), and the deck's own `read()` snapshot (`deck.rs:138`).
- The channel group strings are defined four times: `controls.rs:139`, `midi.rs:45`, `deck.rs:251`, `app.rs:1291`.
- `EQ_KEYS` is defined again at `mixer.rs:45`.

**Recommendation.** A `ControlSpec` registry in core. Each entry holds:
- an id,
- a kind: button, toggle or continuous,
- a `Range`,
- a getter and a setter.

Widgets, MIDI and the keymap all read from it. An effects panel is then a loop over specs.

### F3. Jump and loop sizes are stored twice

**Problem.**
- `DeckUi.jump_size/loop_size` (`deck.rs:240-241`) duplicate `Inner.jump_size/loop_size` (`player.rs:39-40`), which is written at `controls.rs:267,271`.
- If MIDI or a mapping sets `beatjump_size`, the deck UI keeps showing the old size.
- The step-up/step-down logic (`deck.rs:204-225`) exists only in the UI, so the keyboard and MIDI can't step sizes.

**Recommendation.** Give the sizes one owner: either the engine's `DeckShared` or a core `DeckModel`. Add `beatjump_size_up/down` commands.

### F4. Business logic lives in `app.rs` and `deck.rs`

**Problem.** These belong in core but are written in UI files:

| Logic | Where |
|---|---|
| Choosing the reference track for suggestions, with settling and caching. Builds three `format!` stamp strings every frame (`app.rs:499-506`). | `app.rs:460-546` |
| Play-history rules | `app.rs:437-456` |
| Choosing which deck a load goes to | `app.rs:380-384` |
| Folding analysis events into track state, using string states `"queued"`/`"running"`/`"done"` | `app.rs:719-763` |
| Preview lifecycle | `app.rs:550-637` |
| Flow persistence and CRUD, with raw `rusqlite` in the UI | `app.rs:252-370` |
| Grid BPM step math | `deck.rs:1010-1034` |
| Bar/beat counting: an O(beats) scan every frame. Belongs on `Beatgrid`. | `deck.rs:451-466` |
| Scrubbing, done as pause + repeated `playposition` seeks + resume | `deck.rs:415-435` |

**Impact.** None of this can be tested without egui. A future Prep or Library mode would need the same logic.

**Recommendation.**
- Move it into core services: `Session` (decks, history, reference), `PreviewService`, `FlowStore`, `AnalysisState`.
- Make scrubbing an engine command (`Scratch{deck, velocity}`). Slip mode needs that command anyway.

### F5. The domain model is defined in the UI layer

**Problem.**
- `library_ui::Track` (analysis state, key, energy) is imported by the deck (`deck.rs:23`) and by the app.
- Session history, `played: HashSet<i64>`, lives on `LibraryUi`.

**Recommendation.** Move `Track` and `played` into core. This overlaps with the data-model and library reports.

### F6. Async plumbing is inconsistent

**Problem.** Five different patterns are used side by side:

- The `Sink`/`Event` channel (`core/mod.rs:96-120`).
- `Arc<Mutex<Option<..>>>` polled every frame (`app.rs:64,782`; `audio_ui.rs:27`).
- `Arc<AtomicBool>` (`app.rs:125`).
- egui temp data (`library_ui.rs:76`; `mixer.rs:274` for the limiter hold).
- A bare `std::thread::spawn` per operation (`app.rs:375,388,565,875`).

Two related gaps:

- `Event::Midi.port` is ignored (`app.rs:777`).
- Errors only go to `eprintln!` (19 times in `app.rs`). The user never sees them.

**Recommendation.** Route everything through `Event`. Add a `Notifications` model: toasts, plus an errors pill.

### F7. Two decks are hard-coded throughout the UI

**Problem.**
- `[DeckUi; 2]` (`app.rs:39`) and `[Option<Instant>; 2]` (`app.rs:53`).
- `0..2` and `[0,1]` loops (`app.rs:440,245,1093,1125`).
- Mirroring is decided by `index == 0` (`deck.rs:496,512,724`).
- The mixer layout assumes two decks (`mixer.rs:149-158`).

The engine already has a `DECKS` constant.

**Recommendation.** Make the UI generic over `DECKS`. Pass the layout side (left or right) explicitly instead of inferring it from the deck index.

### F8. Persisted settings are scattered

**Problem.**
- Four separate eframe storage keys (`app.rs:74-79`), saved at `app.rs:1073-1078`.
- Tempo ranges are engine state, but they are saved as UI preferences and read back from atomics (`app.rs:241-246`).
- The Flow is saved twice: in eframe storage and in the DB (`app.rs:190-194,252-276`; the eframe copy is a legacy path).
- Panel sizes rely on egui remembering panel ids (`"top"`, `"waves"`, `"decks"`, `"tree"`, `"suggest"`).

**Recommendation.**
- One `UiPrefs` struct under one key, holding the mode, the layout for each mode, and zoom.
- Deck and audio settings go in a core `Settings`, stored in the DB or a file.

### F9. Theming is half done

**Problem.**
- Colours are defined in three places: the palette in `util.rs:5-29`, control colours in `controls/mod.rs:364-374`, and `theme()` in `ui/mod.rs:55`.
- Raw RGB literals appear elsewhere:
  - `deck.rs:206-208,625,630,1173`
  - `wave.rs:149,221,224,321,327`
  - `mod.rs:61,64,67`
  - `button.rs:88`
  - `meter.rs:75`
  - 12 in `flow.rs`
- Hot cues are labelled `1-8` on the pads (`deck.rs:822`) but `A-H` on the waveform (`wave.rs:262`). The waveform labelling would also panic with more than 8 cues.
- Frame and pill styling is copied and pasted (`app.rs:1146-1150,1271-1286`; `deck.rs:481-485`).

**Bug.** `analysis_pill` has a dead branch: both arms of the `if` return `WARN` (`app.rs:978-982`).

**Recommendation.** A `ui/theme.rs` with a `Theme` struct containing:
- the palette,
- deck and hot-cue colours,
- spacing,
- frame builders such as `panel_frame()` and `pill()`.

### F10. Performance

**Repainting never stops.** `ctx.request_repaint()` is called unconditionally (`app.rs:1267`). The app redraws at the display refresh rate even when nothing is happening, including in full-screen Flow.
- **Fix:** repaint only while playing, previewing, scrubbing or analysing. At other times, let `Sink` events wake the UI.

**The player mutex is contended.** It is locked about 5–7 times per frame:
- `deck_info` twice (`app.rs:1003`),
- `on_deck` up to 3 times (`app.rs:464`),
- `track_plays` (`app.rs:449`),
- `routing` (`app.rs:1204`).

The same mutex is shared with the MIDI, load and analysis threads.

**SQLite runs on the UI thread.** `edit_grid` writes to SQLite while holding the player lock (`player.rs:302-341`, called from `app.rs:1223`).

**Allocations every frame:**
- `deck_info` clones the title, artist and source strings (`app.rs:1007-1019`).
- The keyboard handler clones the event list with `events.clone()` (`app.rs:798`).
- 16 hot-cue tooltip `format!` calls (`deck.rs:823-830`).
- `Range::tooltip` is built eagerly for every knob (`controls/mod.rs:546`). Use `on_hover_text_at_pointer` with a closure, or check `hovered()` first.
- The gain tooltip (`mixer.rs:211`).
- `String` keys in `Action::Control` (`deck.rs:297`).
- A new `Mesh` every frame (`wave.rs:163`).

**Waveforms are built on the CPU every frame.**
- Each deck draws about 2 quads per column (`wave.rs:164-210`). That is fine for 2 decks.
- Columns are logical points, so on HiDPI screens the waveform is drawn at half resolution.
- For 4 decks, or Prep's zoomed views, consider either:
  - a wgpu paint callback that samples the detail data as a texture, or
  - cached tiles for each zoom level.

### F11. Testability

- `EarshotApp::new` starts real audio, MIDI, the DB and threads (`app.rs:150-221`). Because of that, the app-level kittest tests are `#[ignore]` and need a real library (`app.rs:1400-1496`).
- There are no tests for `controls::apply`, the MIDI table or the key map.
- The widget tests are good (`knob.rs:141-247`, `deck.rs:1208`).

**Recommendation.** Inject a `Services` struct (engine, DB, MIDI), with a fake `EngineView` for tests. Command dispatch can then be unit-tested: commands go in, state comes out.

## 3. Proposed target structure

```
core/
  command.rs   enum Command { Deck(DeckId, DeckCmd), Mixer(MixerCmd), Browse(BrowseCmd),
                 Library(LibCmd), Flow(FlowCmd), Preview(PreviewCmd), Audio(AudioCmd),
                 Edit(EditCmd), Undo, Redo, View(ViewCmd) }
               enum DeckCmd { Play(Press), Cue(Press), Hotcue{slot,press}, BeatJump{beats},
                 JumpSize(Step), Loop.., Slip(bool), Scratch{vel}, Fx{unit,param,value}, Grid(GridOp) }
  bus.rs       ControlSpec { id: ControlId, mixxx: (&str,&str), kind: Button|Toggle|Continuous(Range),
                 get: fn(&EngineView)->f64, to_cmd: fn(f64)->Command }
               Bus { player, ui_tx } ; fn send(&self, Command)   // thread-safe, MIDI calls it directly
  session.rs   Session { decks: [DeckModel; DECKS], history, last_started, reference() }
  undo.rs      trait Edit { fn apply(&self,&mut Core)->Result; fn revert(..) } ; UndoStack
  model/       Track, AnalysisState(enum), Library, FlowStore, PreviewService
  input/       keymap.rs (data file -> (Key,mods)->ControlId; help generated from it), midi/
ui/
  app.rs       Shell { core: CoreHandle, view: ViewState, prefs: UiPrefs, toasts }
               fn dispatch(&mut self, Command)  // only mutation point
  theme.rs     Theme { palette, deck: [Color32;N], hotcue, frames() }
  views/       perform.rs, prep.rs, library.rs   enum Mode { Perform, Prep, Library }
  panels/      deck/{transport,pads,loops,tempo,grid_editor,fx}.rs, mixer.rs, waveform.rs,
               browser.rs, suggest.rs, flow/
  widgets/     knob, fader, hw_button, meter, key_chip, pill, bound(spec)
  modals.rs    enum Modal { TagEditor(TrackIds), BulkEdit(..), Audio, Keys } (one stack)
  prefs.rs     #[derive(Serialize)] UiPrefs { mode, layouts: HashMap<Mode,Layout>, wave_zoom }

trait Panel { fn ui(&mut self, ui: &mut Ui, cx: &mut Cx); }
struct Cx<'a> { core: &'a CoreView, engine: &'a EngineFrame /* one snapshot per frame */,
                theme: &'a Theme, out: &'a mut Vec<Command> }
```

### How the requested features would fit

- **Effects panel.** Add engine effect commands and effect `ControlSpec`s. The panel is then `for spec in fx_specs(deck) { bound(ui, spec, cx) }`, and MIDI mappings come for free.
- **Slip mode.**
  - Add a shadow playhead in the engine and a `DeckShared.slip_pos` field.
  - Add `Marks.ghost` to the waveform to show it.
  - Replace pause-and-seek scrubbing with `DeckCmd::Scratch`.
- **Prep and Library modes.** A `Mode` enum picks a view, and each view is built from panels. Panel state is saved per mode in `UiPrefs`.
- **Modals.** The shell holds one `Modal` stack. Editors send `Command::Edit`.
- **Undo/redo.** Grid operations and metadata edits become `Edit` objects in core. Each captures what it replaced: the previous `Arc<Beatgrid>` or the previous row. Undo then works the same from the keyboard, MIDI and menus.

### Reading engine state

Read the engine once per frame into an `EngineFrame` (deck snapshots, meters, track ids), without taking the player mutex. Publish `track_id` and the generation counter through atomics or an `ArcSwap`.

## 4. Issues for other areas

- **Library.**
  - `Track`, `played` and the analysis state model live in `ui/library_ui.rs`.
  - Analysis states are strings (`app.rs:722`).
  - The thumbnail, preview and art caches are fields on `LibraryUi`.
- **Core and player.**
  - `Inner` is one mutex shared by the UI, MIDI, load, preview and analysis threads.
  - `edit_grid` does DB I/O while holding it.
  - Grid provenance uses string literals: `player.rs:244-246`, and `"user"` at `player.rs:336`.
  - Splitting state between `DeckSlot` and `DeckShared` forces the UI to take the lock just to read `track_id`.
- **Engine.**
  - Slip, scratch and effects need engine commands.
  - `DeckShared` should expose the jump and loop sizes.
  - The nudge amount is hard-coded at 0.04 in `controls.rs:252`.
- **Design doc.** `docs/design.md` says the keyboard mapping is a file and that `[Suggest]` is its own control group. In the code, the key map is hard-coded and `[Library] TogglePane` stands in for the `[Suggest]` group.
- **Flow.**
  - Flow CRUD and SQL are in `app.rs:310-370`.
  - The Flow is saved in two places.
