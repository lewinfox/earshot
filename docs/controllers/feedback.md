# Controller feedback: LEDs, meters and watching controls

**Summary:** sending feedback to controllers is a moderate job, not a hard one.
earshot already publishes almost everything LEDs need in a snapshot that any
thread can read. The work is a loop on the controller thread that reads it about
100 times a second, spots what changed, and sends MIDI.

Feedback matters to nearly every controller: 137 of the 159 Mixxx mappings send
something back to the device.

## How Mixxx does it

In Mixxx, every control is an observable value (a `ControlObject`). Feedback is
just watching values:

- **XML `<output>`** (2 290 of them, in 61 mappings). Each output watches one
  control (`MidiOutputHandler`, `src/controllers/midi/midioutputhandler.cpp`). On
  every change, Mixxx works out a byte: `<on>` if the value is between `<minimum>`
  and `<maximum>`, otherwise `<off>`. It sends that byte only if it differs from
  the last one sent. That's all: fixed messages, two states, no maths.
- **`engine.makeConnection(group, key, fn)`** (35 mappings, plus 70 using the
  older `connectControl`). Mixxx calls `fn(value, group, key)` on the controller
  thread whenever the control changes. The function usually does maths and calls
  `midi.sendShortMsg`. Calls are queued in order, so every change is delivered.
- **`engine.makeUnbufferedConnection`** (10 mappings). The same, but if changes
  arrive faster than the script keeps up, only the latest is delivered
  (`skipSuperseded`, `src/control/controlobjectscript.cpp`). This is for VU meters.
- **`engine.trigger(group, key)`** and **`connection.trigger()`** (24 mappings).
  These call the connections now with the current value. Scripts use them in
  `init` to paint the starting state.
- **Timers** (`engine.beginTimer`, 89 mappings). Used for blinking and for
  animations.
- **`[App] indicator_250ms` / `indicator_500ms`**. Shared blink clocks, so every
  blinking LED is in step with Mixxx's own `play_indicator` / `cue_indicator`.
- **Meters.** Mixxx updates `vu_meter` 30 times a second (`kVuUpdateRate`,
  `src/engine/enginevumeter.cpp:10`). Scripts then scale it to the device's LED
  ladder. The DDJ-400 does `midi.sendShortMsg(0xB0, 0x02, value * 150)`.
- **Soft takeover.** This is the input side of the same problem.
  - When a knob's hardware position and the software value disagree (sync moved
    the tempo, or the mouse moved the EQ), Mixxx ignores the knob until it comes
    within 3/128 of the value, or crosses it (`SoftTakeover::willIgnore`,
    `src/controllers/softtakeover.cpp`).
  - 47 mappings turn this on from JS and 16 from XML.

Altogether, 109 mappings watch controls from JS, and 121 send MIDI (or HID) from
JS. They watch 171 different controls. These are the most watched, counting XML
outputs, connections, and `components` classes:

| Control | Mappings | earshot today |
|---|--:|---|
| `play` | 65 | `DeckSnap.playing` |
| `pfl` | 64 | `DeckSnap.pfl` |
| `play_indicator` | 55 | worked out from `playing` + cue rules |
| `loop_enabled` | 55 | `DeckSnap.loop_active` |
| `cue_indicator` | 51 | worked out from `playing`, `position`, `cue_point` |
| `sync_enabled` | 42 | `DeckSnap.sync` |
| `vu_meter` (+ `_left` / `_right`: 18) | 40 | `DeckSnap.vu` (mono peak) |
| `playposition` | 32 | `position / duration` |
| `keylock` | 31 | `DeckSnap.keylock` |
| `track_loaded` | 30 | `DeckSnap.track_id.is_some()` |
| effect `enabled` | 30 | no effects yet |
| `hotcue_N_enabled` / `_status` | 28 / 18 | `DeckSnap.hotcues[n].is_some()` |
| `rate` | 22 | `DeckSnap.fader` |
| `cue_default` | 22 | held state: the input side knows |
| `beat_active` | 21 | worked out from the grid + `position` |
| `hotcue_N_color` | 16 | hot cues have no colour yet |
| `quantize` | 14 | `DeckSnap.quantize` |
| `loop_start_position` / `loop_end_position` | 13 | `DeckSnap.loop_in` / `loop_out` (seconds → samples) |
| `filterLowKill` etc. | 13 | no EQ kill buttons yet |
| `duration` | 13 | `DeckSnap.duration` |
| `peak_indicator` | 12 | `DeckSnap.vu ≥ 1` |
| `beatloop_N_enabled` | 12 | `loop_active` and the loop's length in beats |

So the engine side is mostly there. Almost everything in this list is either a
field of `EngineSnapshot` (`engine/src/snapshot.rs`) or worked out from one. The
gaps are effects, EQ kills, hot cue colours, and stereo meters.

## What earshot has to build on

- **`Monitor::snapshot()`** (`engine/src/snapshot.rs:269`) returns the state as of
  the last audio callback, through a triple buffer.
  - `Monitor` is cheap to clone and safe to read from any thread. The audio
    thread never takes its lock.
  - The snapshot is a few KB of plain `Copy` data, so reading it 100 times a
    second costs nothing.
- **`EngineEvent`s** (`snapshot.rs:168`) report seven kinds of edge: loaded,
  started, stopped, ended, cue set, hot cue set, loop set.
  - There is only one consumer: `Player::catch_up` drains them on the UI thread
    (`gui/src/core/player.rs:86`).
  - They are for keeping the app's own state in step, not for LEDs.
- **App state the engine doesn't hold**: the beat jump and loop sizes, the beat
  grid (needed for `beat_active`), the track's key, BPM and colour, and the
  library's focus. These live in `DeckSession` and the UI. A feedback loop off the
  UI thread needs them published, for example as an `Arc` swapped in whenever
  they change.
- **The registry already has getters.** `DeckControl::get(&DeckSnap)` and
  `Control::get(&EngineSnapshot)` (`gui/src/core/bus.rs:471`, `:709`) were written
  "for widgets and controller LEDs".
- **`midir` does output too.** `MidiOutput` opens a port and its
  `MidiOutputConnection::send` takes raw bytes. Pair the output port with the
  input port by name.

## Three ways to watch for changes

| | A. Diff on the UI thread, once per frame | B. Engine events for every change | C. Poll the snapshot on the controller thread (recommended) |
|---|---|---|---|
| How | In `App::update`, compare the frame's snapshot with the last one; send MIDI | Add an event per control change on the audio thread; a consumer sends MIDI | Each controller thread wakes every ~10 ms, takes `Monitor::snapshot()` and the app-state `Arc`, recomputes the watched cells, sends what changed |
| Latency | one frame (16 ms), or none at all when egui isn't repainting (window hidden, idle) | lowest | ≤ 10 ms + one audio buffer |
| Stalls | LEDs freeze when the UI stalls (a long frame, a modal, a big library query) | none | none: independent of the UI and the audio thread |
| Audio thread cost | none | more work per callback; the event ring can overflow (`Stats::dropped_events`); continuous values (meters, position) don't suit events | none |
| Fits the JS | badly: JS runs on the controller thread, so changes would have to cross threads | badly: second consumer of a single-consumer ring | well: changes are found on the same thread that runs the callbacks |

**Pick C.** It needs nothing new from the engine. It keeps the UI out of the
path. It puts change detection on the thread that owns the JS and the MIDI
output. Events stay what they are: the app's way of tracking cues and loops.

One difference from Mixxx to note:

- **Polling sees only the latest value at each tick.** Mixxx's `makeConnection`
  promises every intermediate value. With polling, both connection kinds deliver
  "the latest value, at most once per tick".
- **That's fine for LEDs**, and it's what `makeUnbufferedConnection` already
  does.
- **Values set from the controller itself are the exception.** If a script sets a
  value and expects its own connection to fire, fire it straight away from the
  `set`, through the shadow value (see [mixxx-mappings.md](mixxx-mappings.md)), as
  well as from the poll.

## The loop, concretely

The controller thread already exists (see [mixxx-mappings.md](mixxx-mappings.md)):
it runs input, JS and timers. Feedback is one more job on it:

```rust
// Every TICK (10 ms), and right after handling input:
let snap = monitor.snapshot();          // engine state, lock-free for the audio thread
let app = app_state.load();             // Arc<AppState>: grids, sizes, track info
for cell in table.watched() {           // only cells an <output> or a connection uses
    let v = cell.value(&snap, &app, now);   // getter, derived rule, shadow or stub
    if cell.last != Some(v) {
        cell.last = Some(v);
        for out in &cell.outputs { out.maybe_send(v, &midi_out) }   // XML: on/off, dedup
        for c in &cell.connections { js.call(c, v, cell.group, cell.key) }
    }
}
```

**Derived cells** (`play_indicator`, `cue_indicator`, `beat_active`,
`indicator_250ms`) are small functions of the snapshot, the app state and a
monotonic clock:

- **Blinking** runs off one shared clock, so all blinks stay in step.
- **`play_indicator` and `cue_indicator`** copy Mixxx's "Pioneer" cue-mode rules
  (`CueControl::updateIndicators`, `src/engine/controls/cuecontrol.cpp:2281`).
  When paused away from the cue point, the cue LED blinks fast. When paused on
  it, the cue LED is lit. While playing, the play LED is lit. This is the CDJ
  behaviour earshot's cue already has.
- **`beat_active`** is on for the first part of each beat. It needs the deck's
  grid in `AppState`.

**Rate limits:**

- Only send a byte when it changes, per (status, data1), like Mixxx.
- Recompute meter cells at 30 Hz, not every tick, to match Mixxx. Scripts were
  tuned for that rate, and VU messages are most of the traffic.
- Keep a per-device budget (say 1 000 messages a second) with newest-wins
  coalescing per (status, data1). A slow DIN-MIDI device never queues stale
  updates.

**Start and stop:**

- **On connect:** run `init`, then trigger every output and connection once, so
  the device shows the real state at once. Some controllers also answer a SysEx
  query with their knob positions; the DDJ-400's `init` sends one. Soft takeover
  ignores those first values until the knobs are moved, as Mixxx does.
- **On disconnect or quit:** run `shutdown`, which turns the LEDs off, before
  closing the port.

**Soft takeover** sits beside this loop on the input side:

- Each knob cell keeps the last hardware value. It accepts a new one only within
  3/128 of travel of the current value, or when the knob crosses it. This uses
  Mixxx's rules, including the 50 ms "keep accepting" window.
- earshot's tempo fader already has a "pickup" rule of its own in the engine
  (`DeckCmd::TempoFader`, `DeckSnap::fader_live`). Keep that for `rate` and use
  the general one for everything else.

## How hard is it

Rough sizes, not promises:

| Piece | Size | Depends on |
|---|---|---|
| Open the output port; send; dedup; per-device budget | ~150 lines | midir |
| Polling loop and watched-cell table, XML `<output>`s for mapped cells | ~250 lines | the control table ([mixxx-mappings.md](mixxx-mappings.md)) |
| Derived cells: indicators, blink clocks, `beat_active`, meters at 30 Hz | ~200 lines | `AppState` published by the UI (grids, sizes) |
| JS connections: `makeConnection`, `connectControl` (including string callbacks), `makeUnbufferedConnection`, `trigger` | ~200 lines | the JS engine |
| Soft takeover | ~100 lines + tests | |
| Stereo meters (`vu_meter_left/right`), hot cue colours, EQ kills | engine and app features | listed in [mixxx-controls.md](mixxx-controls.md) |

## Benefits and costs

**Benefits:**

- **The controller tells the truth.** The play, cue, sync and loop LEDs and the
  pads show what earshot is doing, not what the last button press guessed.
  Without feedback, the DDJ-400's pad and button LEDs don't follow earshot. Its
  Mixxx mapping drives 112 of them; a hot cue set on screen never shows on the
  pads.
- **Mixxx's mappings already contain this work** for 137 devices. We inherit it
  by implementing about 6 API functions and one output rule.
- **No changes to the audio thread.**

**Costs:**

- **One more thread per controller**, waking 100 times a second. That's
  negligible CPU, but it's one more thing running.
- **An `AppState` to publish from the UI.** It's a small piece of plumbing, but
  it's new shared state that has to stay up to date.
- **The latest value, not every value.** This is invisible on LEDs. A script that
  counts changes (none found) would see fewer.
- **Behaviour we copy, we own.** The cue-mode blink rules and the soft-takeover
  rules come from Mixxx. If we change cue behaviour, the indicators have to
  follow.

## Plan

1. **XML outputs for what's mapped today.** Poll, diff and send for `play`,
   `pfl`, `sync_enabled`, `keylock`, `quantize`, `loop_enabled`,
   `hotcue_N_enabled` / `_status`, `beatloop_N_enabled`, `track_loaded`. On a
   DDJ-400 that lights the pads, play, cue, sync, headphone cue and loop buttons
   with no JS at all.
2. **Derived cells.** `play_indicator`, `cue_indicator`, `beat_active`, the blink
   clocks and meters. Publish `AppState` from the UI.
3. **Connections, with the JS engine.** Everything in the JS column comes with
   phase 2 of the mapping work ([README.md](README.md)). The DDJ-400's VU meters,
   loop blink, sampler pads and FX LED all live here.
4. **Soft takeover** for every knob.
5. **Initial state** on connect, LEDs off on quit.
