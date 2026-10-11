# Engine and the engine–app boundary

Part of the [October 2026 architecture review](README.md). Line references are to `8bb7c31`.

## 1. How it works today

- **Engine crate.** `Engine` (`engine/src/lib.rs:29`) owns:
  - an `rtrb` command producer with 1024 slots,
  - a garbage ring with 64 slots, emptied by a GC thread,
  - an `Arc<Shared>`.
- **Renderer.** `Renderer` (`engine.rs:511`) runs on the cpal thread, or on a timer thread when there is no audio device (`output.rs:262`). It holds:
  - `[Deck; 2]`,
  - four arrays that run alongside the decks: `deck_buf`, `keylocks`, `strips` and the limiters,
  - a separate preview player (`engine.rs:524-527`).
- **Commands.** One flat `Command` enum with 35 variants (`engine.rs:206-352`), applied in a 280-line `match` (`engine.rs:904-1186`).
- **State going back to the app.** The only way out is `publish()` (`engine.rs:1188`). After every callback it writes about 35 `Relaxed` atomics per deck into `Shared`. Nothing sends events from the engine to the app.
- **App side.** `Player = Arc<Mutex<Inner>>` (`player.rs:36-61`) holds:
  - the `Engine`,
  - the app's own copy of what is loaded on each deck (`DeckSlot`),
  - the jump and loop sizes.

  The UI thread, the MIDI callback thread, the loader threads and the analysis threads all share this one mutex.
- **Control bus.** `controls.rs` parses Mixxx `[Group] key` strings into `Command`s. Three sources use it, all through `controls::apply`:
  - the keyboard (`app.rs:1290`),
  - on-screen controls (`deck.rs:296`, via `Action::Control{key: String}`),
  - MIDI (`midi.rs:47`).

**What is already good.**
- Real-time rules are mostly followed: tracks are decoded off the audio thread, freeing goes through the garbage ring, and the DSP code doesn't allocate.
- The renderer tests are good (the `Rig` at `engine.rs:1284`, 17 tests).

The problems are in structure and ownership, set out below.

## 2. Findings, most important first

### F1. The engine is the only owner of performance state, and an audio restart wipes it

**What happens.** `start_engine` (`player.rs:171-191`) builds a new `Renderer` and resends only two things:
- `Load`, with `generation: 0`, which no longer matches `DeckSlot.generation`,
- `SetRouting`.

Everything else is lost:
- hot cues, the cue point, position and play state,
- loops,
- EQ, volume, crossfader and gains,
- keylock, quantize and sync,
- auto-gain. `AutoGain` is never resent; compare `player.rs:270`.

Tempo ranges are restored by hand in `app.rs:647-655`.

**Why hot cues can't be saved.** They exist only as `[Option<f64>; 8]` inside the engine (`engine.rs:391`). This is the cause of the README's "Hot cues are not saved".

**Why it matters.** Saved hot cues and memory cues, restoring a session, and recording all need the control side to own this state.

**Fix.**
- Keep the authoritative deck and mixer state on the control side.
- Have the engine send an event whenever it changes that state.
- On restart, send `Restore(Box<EngineState>)`.

### F2. The engine has no event channel back to the app

**What happens.** Changes made on the audio thread show up only as atomics that the app polls. Examples:
- a hot cue being set (`engine.rs:1060`),
- the cue point being set (`:973`),
- a loop being set,
- the end of the track (`:735-740`).

**Why it matters.** Saving cues and loops, keeping set history, and controller LED feedback all need to know the moment something changes. Today the "played for 30 s" rule is worked out by the UI from polled values.

**Fix.** Add a second `rtrb` ring that carries `EngineEvent` values. They should be `Copy` and need no allocation. Tag each with the deck's `generation` so stale events can be dropped.

### F3. Published state can be read half-updated, and two generation counters disagree

**Half-updated reads.** Each field is a separate `Relaxed` atomic (`engine.rs:48-94`). The UI's `read()` (`deck.rs:138`) can see the new track's `generation` alongside the old track's `position`.

**Two generation counters.**
- `DeckSlot.generation` (`player.rs:33,262`) belongs to the app.
- `DeckShared::generation` belongs to the engine.

The UI draws the waveform and grid from `DeckSlot` straight away, but takes the position from the engine, which may not have applied `Load` yet.

**Hard-coded cue count.** `HOTCUES` is a constant, but `deck.rs:130,139` still hard-codes `[_; 8]`.

**Fix.**
- Publish one `Copy` `EngineSnapshot` per callback through a triple buffer (the `triple_buffer` crate), plus perhaps one atomic for the playhead.
- Include in the snapshot the generation and track id it belongs to.

### F4. One mutex guards the command queue, including on the MIDI path

**What happens.** `rtrb::Producer` needs `&mut`, so every sender has to lock `Player`. The senders are MIDI (`midi.rs:188`), the UI (`app.rs:398`) and analysis (`analysis.rs:410`).

**The worst case.** `edit_grid` locks the player at `player.rs:303` and then writes to SQLite at `:341` while still holding it. It runs on the UI thread (`app.rs:1222`). While that write is in progress, MIDI and keyboard input are blocked.

**Fix.**
- Give each source (UI, MIDI, loader) its own single-producer ring and have the renderer drain all of them. Alternatively, put the producer behind its own small lock.
- Keep deck metadata out of that lock.
- Never do database I/O while holding it.

### F5. There is no single set of actions: everything is strings, and toggles race

**String parsing on every event.** `controls::apply` (`controls.rs:45-178`) parses strings for each event and allocates while doing it:
- `key.into()` at `deck.rs:300`,
- `format!("hotcue_{}_clear")` at `deck.rs:840`,
- `control.replace(...)` at `app.rs:866`.

An unknown key fails only at runtime.

**Toggles are written three times.** Each copy reads the current state from a snapshot that can be one callback old, then writes the opposite. Two quick presses can both read the same state.
- MIDI `Kind::Toggle`: `midi.rs:191-197`.
- Keyboard: `app.rs:844-848`.
- On-screen: `deck.rs:808,1086,1098,1108`.

**Some actions skip the bus.** These go straight to the engine:
- `Action::TempoRange` sends `Command::TempoRange` (`app.rs:235`; the `Action` enum is at `deck.rs:27`),
- grid edits,
- load,
- preview.

**Jump and loop size are held twice:** in `DeckUi.jump_size/loop_size` (`deck.rs:240-241`) and in `Inner.jump_size/loop_size` (`player.rs:40-41`). A MIDI `beatjump_size` won't update the UI.

**Fix.**
- Make a typed `Control` enum the shared set of actions.
- Treat Mixxx strings only as an import/export format (`Control::from_mixxx`).
- Make toggles engine commands (`Set(bool)` or `Toggle`), applied on the audio thread so no read-modify-write is needed.
- Make jump and loop size part of the engine's deck state.

### F6. The deck has no transport layer, which blocks slip, reverse, censor and roll loops

**The playhead is changed directly in many places.** `Deck.pos` is written in about 12 places:
- `engine.rs:731-741`, in the render loop, where loop wrap and end of track are handled inline,
- `966, 974, 980, 1036, 1049, 1062, 1071`,
- `seek` at `:494`.

**Speed is assumed to be positive.**
- `step = deck.speed/sr` at `:700`.
- The end-of-track check only looks forward.
- Keylock `SPEEDS` covers only `0.5..=2.0`.

**Loops are loose.** A loop is three separate fields (`:388-390`). `BeatLoop` turns off any active loop whatever its size (`:1000-1003`).

**Cue preview is mixed into `apply`.** Holding cue or a hot cue to preview is a small state machine (`Preview`, `:363`) written inside the command `match`.

**Fix.** Add a `Transport` struct:
- every move goes through `jump(to, Reason)`,
- an optional `slip_pos` keeps advancing in the background,
- a signed `direction` allows reverse,
- a `LoopState` records what kind of loop it is.

### F7. Two decks are hard-coded throughout

**In the engine.**
- The sync leader is always "the other deck", `1 - d` (`engine.rs:833,883,1109`).
- Crossfader gains use `xf[d]` with `xf: [f32; 2]` (`:760,775`).
- `Renderer::new` uses fixed-size array literals (`:609-632`).

**In the app.**
- `CHANNELS`, `EQ_GROUPS` and `FILTER_GROUPS` (`controls.rs:19-29`).
- `CH` (`midi.rs:45`).
- `deck.rs:251`.
- `[0,1]` at `app.rs:241`.
- `deck_key` (`app.rs:1290`).

**A third playback path.** The preview player (`engine.rs:796-812`) has its own sample-reading loop and no keylock or loops. A sampler would make a fourth.

**Fix.**
- One `Player` playback component, used by decks, preview and sampler slots, and routed to channels.
- Sync follows an explicit leader (`SyncLeader: Option<DeckId>`) rather than "the other deck".

### F8. The mixer is a fixed chain, and parameter smoothing is uneven

**Fixed chain.** The signal goes `Strip` (gain → `Eq3` → `DjFilter`, `engine.rs:542-590`) → crossfader → `0.8*m` → limiter. There is no processor trait, so adding effects means adding more fields and more `Command` variants.

**Smoothing differs by control.**

| Control | How it changes |
|---|---|
| Pregain, EQ | `Smoothed`, per sample |
| Filter | Recomputed once per block (`dsp.rs:177`; blocks can be 4096 frames). State is reset when the mode changes (`dsp.rs:186-191`). |
| Volume, crossfader, main and head gain, balance | Applied raw (`engine.rs:759-787`) |

The raw controls will cause zipper noise on MIDI faders.

**Fix.** A `Processor` trait with indexed, smoothed parameters, and a `ParamId` scheme for addressing them. MIDI, the keyboard, the UI and any later automation then share one path.

### F9. A command with a bad deck index panics on the audio thread

- Load, Play, Cue, Seek and others index with `self.decks[deck]` (`engine.rs:912,946,959,985…`).
- Other commands use `get_mut` (`:1134,1139,1147,1153`), so handling is inconsistent.
- A panic in the cpal callback kills the audio.

**Fix.** A `DeckId` newtype, checked where commands are sent.

### F10. `player.rs` does too many jobs

**`load_track` (`player.rs:195-286`) does all of this:**
- decodes the file,
- writes energy and loudness to the DB,
- migrates old grids (`:215-224`),
- computes waveforms,
- queues analysis,
- sends `Load` and then `AutoGain` as two separate commands.

Because the last two are separate, one block plays at gain 1.0 before `AutoGain` arrives. Auto-gain should be a field of `Load`.

**Grid source is a string:** `"analysis" | "user" | "tempo-lock sidecar" | "BPM tag (placeholder)" | "none"` (`player.rs:254,386-388`).

**`update_decks` (`analysis.rs:405-424`)** swaps a deck's grid without checking `grid_source == "user"`.

*Note added during synthesis:* the caller does guard this with `keep_user` (`analysis.rs:352`). But `keep_user` is decided from a grid read when the job starts, so the race described in the [README](README.md) (confirmed bug 1) remains.

### F11. Testability

- `controls.rs`, `midi.rs` and `player.rs` have no tests.
- The deck rules (cue, hot cue, loop, beat jump) live in `Renderer::apply`, so they can only be tested by rendering audio.
  - **Fix:** move them into plain `Deck` methods and the mapping into plain `Control` functions. Both can then be unit-tested without a renderer.
- The engine crate also contains offline analysis code: `energy`, `loudness`, `waveform`, `decode` and `demo`.
  - **Fix:** split it into `earshot-engine` (real-time) and `earshot-audio` (decoding and analysis). That makes the real-time rules easier to enforce.

## 3. Proposed target design

```rust
// ---- identity & vocabulary (engine crate, no strings) ----
#[derive(Copy, Clone, Eq, PartialEq, Hash)] pub struct DeckId(u8);   // validated < n_decks
#[derive(Copy, Clone)] pub enum Bus { Master, Cue, Record }

#[derive(Copy, Clone)] pub enum Btn { Down, Up }
#[derive(Copy, Clone)] pub enum Set { On, Off, Toggle }

#[derive(Copy, Clone)]
pub enum DeckCmd {
    Play(Set), Cue(Btn), Seek(f64), BeatJump(f64), SetJumpSize(f64),
    Hotcue { slot: u8, btn: Btn, shift: bool },
    Loop(LoopCmd), Slip(Set), Reverse(Set), Censor(Btn),
    Tempo(TempoCmd), Sync(Set), Quantize(Set), Keylock(Set, KeylockMode),
}
pub enum LoopCmd { Beat { beats: f64 }, Roll { beats: f64, btn: Btn }, In, Out, Reloop, Scale(f64) }

pub enum Command {                         // what crosses the ring
    Deck(DeckId, DeckCmd),
    Param(ParamId, f32),                   // every knob/fader: EQ, volume, xfader, FX params
    Load { deck: DeckId, track: Arc<LoadedTrack> },   // audio+grid+cues+auto_gain, one atomic swap
    SetGrid { deck: DeckId, generation: u64, grid: Arc<Beatgrid> },
    InstallFx { slot: FxSlot, fx: Box<dyn Processor> }, // built off-thread; old one -> garbage
    SetSyncLeader(Option<DeckId>), SetRouting(Routing),
    Restore(Box<EngineState>),             // after device restart
}
#[derive(Copy, Clone)] pub enum ParamId {
    Channel(DeckId, ChannelParam), Master(MasterParam), Fx(FxSlot, u8),
}
pub struct LoadedTrack { audio: Arc<TrackAudio>, grid: Option<Arc<Beatgrid>>,
                         cues: CueSet, auto_gain: f32, generation: u64, track_id: i64 }

// ---- deck internals ----
struct Transport { pos: f64, dir: f64, slip: Option<f64>, loop_: LoopState, playing: bool }
impl Transport { fn jump(&mut self, to: f64, why: JumpReason) { /* keylock restart, slip */ } }
pub struct LoopState { start: f64, end: f64, active: bool, kind: LoopKind /* Manual|Beat|Roll */ }
pub struct CueSet { main: f64, hot: [Option<Cue>; 16] }   // Cue{pos, len: Option<f64>, kind}

struct Player { track: Option<Arc<LoadedTrack>>, transport: Transport, speed: SpeedState, keylock: Keylock }
// used by Deck, Preview and Sampler slots alike

// ---- processing graph ----
pub trait Processor: Send {
    fn params(&self) -> &'static [ParamSpec];          // name, range, default, smoothing
    fn set_param(&mut self, i: usize, v: f32);          // sets a Smoothed target
    fn process(&mut self, io: &mut StereoBlock);
    fn reset(&mut self);
}
struct Channel { input: ChannelInput /* Deck(DeckId)|Sampler(u8)|Preview */,
                 chain: ArrayVec<Box<dyn Processor>, 8>, // trim, Eq3, filter, FX…
                 fader: Smoothed, xfader: XfAssign, sends: BusMask }
struct Mixer { channels: Vec<Channel> /* sized at build */, master_fx: ArrayVec<..>,
               buses: [StereoBuf; 3], limiters: [Limiter; 2] }

// ---- outbound ----
pub enum EngineEvent { HotcueSet{deck,gen,slot,pos}, CueSet{..}, LoopSet{..},
                       TrackEnded{deck,gen}, Started{deck,gen}, Overrun }
pub struct EngineSnapshot { decks: [DeckSnap; MAX_DECKS], mixer: MixerSnap, perf: PerfSnap } // Copy, triple buffer
```

**On the app side**, `core/controls.rs` becomes:

```rust
enum Control { Deck(DeckId, DeckControl), Mixer(..), Library(..), Suggest(..) }
```

It provides:
- `fn to_command(&self, v: f64) -> Option<Command>`,
- `fn value(&self, snap: &EngineSnapshot) -> f64`, for LED feedback and knobs,
- Mixxx names as an import/export format.

The keyboard map, the MIDI table and the on-screen widgets all produce a `Control`.

**A `DeckSession` model** is owned by the app and saved: cues, loops, gains and tempo ranges. It is updated from `EngineEvent`s, and rebuilt into `EngineState` to send with `Restore`.

## 4. Issues that cross into other areas

- **Data model.** The `cues` table from the design doc (kind, slot, position, length, label, colour) should map one-to-one onto `CueSet`. Load cues with the track, and save them when `EngineEvent`s arrive. Grid `source` should be an enum, in code and in the DB, not a string (`player.rs:254`).
- **Analysis.**
  - `update_decks` (`analysis.rs:405`) needs a check on the generation or track, and a check that it won't overwrite a user grid.
  - `load_track` (`player.rs:203-212`) writes energy and loudness to the DB on every load. That is analysis work hidden inside the player.
- **GUI state.** `DeckSlot` (app) and `Deck` (engine) both record what is loaded, each with its own generation. Merge them under a single `LoadedTrack` and generation. The UI should read one snapshot per frame instead of about 35 atomics.
- **Persistence.** Tempo ranges are saved in eframe storage (`app.rs:79,1077`), and nothing else in the mixer is saved at all. Session and mixer state needs one home.
- **Threading.**
  - The `Player` mutex is shared by the UI, MIDI, loader and analysis threads.
  - Taking the DB lock while holding the player lock (`player.rs:303/341`) sets a lock order that every other area would have to follow.
