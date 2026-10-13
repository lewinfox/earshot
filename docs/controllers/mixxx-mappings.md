# Making Mixxx mappings Just Work

**Summary:** load Mixxx's XML and JS files as they are, at runtime. Run the JS in
an embedded QuickJS. Put a translation layer between them and earshot that turns
Mixxx's `[Group] key` names into earshot controls. Any name earshot doesn't have
yet becomes a "stub": it holds a value, logs once, and never crashes. Don't
convert mappings to our own format. Keep our own format (not XML) for earshot's
own bindings, and for overrides on top of a Mixxx mapping.

Why the JS matters: two thirds of the inputs in Mixxx's own mappings go to a
script function, not straight to a control. 147 of the 159 mappings ship a
script. Without a JS engine, only 12 mappings (8 %) load completely.

## How a Mixxx mapping works

A mapping is one XML file, plus the script files that XML lists. Mixxx 2.5.6
installs 159 of them in `/usr/share/mixxx/controllers`:

- 142 MIDI (`*.midi.xml`)
- 16 HID (`*.hid.xml`)
- 1 "bulk" USB

The rest of the 315 files there are their scripts and four shared libraries.

### The XML

The XML has two lists: **inputs** (`<control>`) and **outputs** (`<output>`).
Here are two inputs from the DDJ-400's file (`Pioneer-DDJ-400.midi.xml`):

```xml
<control>
    <group>[Channel1]</group>
    <key>play</key>                       <!-- a control: set it directly -->
    <status>0x90</status>                 <!-- note on, MIDI channel 1 -->
    <midino>0x0B</midino>
    <options><Normal/></options>
</control>
<control>
    <group>[Channel1]</group>
    <key>PioneerDDJ400.jogTurn</key>      <!-- a JS function: call it -->
    <status>0xB0</status>
    <midino>0x22</midino>
    <options><Script-Binding/></options>
</control>
```

**Inputs** are looked up by status byte and data byte
(`MidiController::receive`, `src/controllers/midi/midicontroller.cpp:300`). Then
one of two things happens:

- **With `<Script-Binding/>`**, the key is the name of a JS function. Mixxx calls
  it as `fn(channel, control, value, status, group)`.
- **Otherwise**, Mixxx converts the 7-bit value by the input's option (table
  below). Then it sets the control "as MIDI would"
  (`ControlObject::setValueFromMidi`). Each control's behaviour then scales the
  value. For example, a toggle button flips on press, and a log pot maps 64 to
  1.0.

These are the input options, counted across all 159 mappings
(`MidiOption`, `src/controllers/midi/midimessage.h:107`):

| Option | What it does to the 0–127 value | Uses | Mappings |
|---|---|--:|--:|
| `Script-Binding` | call the JS function named in `<key>` | 11 284 | 129 |
| `Normal` (or no option) | pass it through | ~5 050 | most |
| `Button` | press = 1, release = 0 | 291 | 17 |
| `Soft-takeover` | ignore the knob until it reaches the current value | 154 | 16 |
| `Spread64` | relative: value − 64 | 80 | 8 |
| `fourteen-bit-msb` / `-lsb` | join two messages into one 14-bit value | 74 + 74 | 16 |
| `Invert` | 127 − value | 40 | 22 |
| `SelectKnob` | relative, two's complement, not added to the old value | 28 | 19 |
| `Diff` | relative, added to the old value | 27 | 5 |
| `Switch` | any message = 1 | 14 | 2 |
| `HercJog`, `HercJogFast`, `Rot64`, `Rot64Fast`, `Rot64Invert` | old jog-wheel encodings | 10 | 2 |

So 5 651 inputs go straight to a control and 11 284 go to a script (67 %). Half
the MIDI mappings send at least 55 % of their inputs to scripts, and 28 send all
of them.

**Outputs** (2 290 of them, in 61 mappings) light an LED from a control. Mixxx
watches the control. Whenever its value is between `<minimum>` and `<maximum>`
(default 1), Mixxx sends the `<on>` byte; otherwise it sends `<off>`. It never
sends the same byte twice in a row (`MidiOutputHandler::controlChanged`,
`src/controllers/midi/midioutputhandler.cpp`). [feedback.md](feedback.md) covers
this.

Thirteen mappings also have a `<settings>` block: options the user sets in
Mixxx's preferences, which scripts read with `engine.getSetting`.

### The JS

- **Where scripts run.** Scripts run in one JS engine per controller (Qt's
  `QJSEngine`), on Mixxx's controller thread (`ControllerManager`, high priority).
  They never run on the audio thread.
- **How a script starts and stops.** Each script file is registered under a
  `functionprefix`, such as `PioneerDDJ400`. Mixxx calls `prefix.init(id,
  debugging)` once at start and `prefix.shutdown()` at the end.
- **Shared libraries.** Four libraries are plain JS built on the same API:
  - `common-controller-scripts.js`: the `script.*` helpers, always loaded;
  - `midi-components-0.0.js`: the `components` library, used by 34 mappings;
  - `lodash.mixxx.js`;
  - `common-hid-packet-parser.js`, used by the HID mappings.

  If we implement the API, all four load unchanged.
- **How much JS there is.** 133 000 lines across 147 mappings. The median
  mapping script is about 590 lines; the DDJ-400's is 701.
- **Which JS features they need.**
  - 75 scripts use arrow functions, 45 use `let`, and 15 use `class`.
  - None use `import`.
  - 51 mappings pass a *string* of code as a callback
    (`engine.connectControl(g, k, "Foo.bar")`), and 4 do the same with
    `beginTimer`. So the engine must be able to evaluate code from a string.

Here is the API those scripts call. It is small. Counts are the number of
mappings whose own scripts call each function, with the total number of calls.
(`tools/mixxx_usage.py`; the full API is in `res/controllers/engine-api.d.ts`.)

| Function | Mappings | Calls | What it does |
|---|--:|--:|---|
| `engine.setValue(group, key, v)` | 139 | 3 615 | set a control, in its own units |
| `engine.getValue(group, key)` | 129 | 2 100 | read a control |
| `midi.sendShortMsg(status, d1, d2)` | 110 | 2 830 | send a 3-byte MIDI message (LEDs) |
| `engine.scratchEnable / scratchTick / scratchDisable` | 100 | ~400 | jog-wheel scratching (see below) |
| `engine.beginTimer / stopTimer` | 89 | 568 | timers (blinking, long press); 20 ms minimum |
| `engine.connectControl` | 70 | 643 | old form of `makeConnection` |
| `engine.softTakeover` | 47 | 331 | turn on soft takeover for a knob |
| `engine.setParameter / getParameter` | 45 / 17 | 282 | the same, as 0..1 knob travel |
| `engine.isScratching` | 42 | 58 | |
| `engine.makeConnection` | 35 | 267 | call a function whenever a control changes |
| `midi.sendSysexMsg` | 29 | 162 | send a SysEx message (init, displays) |
| `engine.trigger` | 24 | 195 | call a control's connections now (initial LED state) |
| `engine.softTakeoverIgnoreNextValue` | 21 | 50 | |
| `engine.brake / spinback / softStart` | 18 / 9 / 3 | 46 | turntable-style stops and starts |
| `engine.getSetting` | 13 | 166 | the mapping's `<settings>` |
| `engine.makeUnbufferedConnection` | 10 | 24 | `makeConnection`, keeping only the latest value |
| `engine.reset`, `getDefaultValue`, `log`, … | ≤ 3 | | |

The `script.*` helpers they use most are `toggleControl` (29 mappings),
`absoluteLin` (24), `deckFromGroup` (20), `triggerControl` (20) and
`absoluteNonLin` (18). These all come free with `common-controller-scripts.js`.

The components classes they use most are `Button` (27 mappings), `Deck` (26),
`ComponentContainer` (24), `Pot` and `Encoder` (22), `PlayButton` and `CueButton`
(19), `HotcueButton` (16), `SamplerButton` (15), `SyncButton` (13) and
`EffectUnit` (12). These come free with `midi-components-0.0.js`.

The 16 **HID** mappings also need `controller.send`, `sendOutputReport` and
`getInputReport`, and a way to read raw HID reports (`hidapi`). Leave these until
last.

### Scratching is part of the API, not a control

`engine.scratchEnable(deck, intervalsPerRev, rpm, alpha, beta)` starts a 1 ms
timer on Mixxx's controller thread. Each tick, the timer runs an alpha-beta
filter over the jog ticks the script has fed in with `scratchTick`. It then
writes the resulting speed to the deck's `scratch2` control
(`ControllerScriptInterfaceLegacy::scratchEnable` / `scratchProcess`,
`src/controllers/scripting/legacy/controllerscriptinterfacelegacy.cpp:683`). 100
mappings scratch this way. 117 use `jog` for pitch bend when the wheel isn't
touched.

earshot needs a scratch mode in the engine for this: "follow this speed" in
place of the tempo, with ramps in and out. The filter can run on the controller
thread like Mixxx's, or in the audio callback.

## The DDJ-400, worked through

The DDJ-400 mapping is 3 044 lines of XML and 701 of JS. It has 201 inputs:

- 101 direct
- 100 to 27 script functions

It also has 112 outputs. Altogether it uses 55 distinct control names:

| Status | Controls |
|---|---|
| **have** (works today) | `play`, `cue_default`, `pfl`, `volume`, `pregain`, `rate`, `quantize`, `sync_enabled`, `hotcue_N_activate`, `hotcue_N_clear`, `beatloop_N_toggle`, `loop_in`, `loop_out`, `reloop_toggle`, `beatjump`, `beatjump_size`, `LoadSelectedTrack`, EQ `parameter1-3`, `super1` (filter), `crossfader`, `headGain`, `headMix`, `MoveVertical` |
| **name only** (earshot can do it) | `loop_enabled`, `loop_scale`, `loop_start_position`, `loop_end_position`, `track_loaded`, `vu_meter`, `hotcue_N_enabled` (pad LEDs), `beatloop_N_enabled` (pad LEDs), `start_play`, `cue_gotoandplay`, `cue_gotoandstop`, `MoveFocusForward/Backward` |
| **partial** | `rateRange` (value vs. button; see [mixxx-controls.md](mixxx-controls.md)) |
| **missing: small** | `beatsync` (the sync button's short press), `reloop_andstop`, `play_indicator`, `cue_indicator` (LEDs) |
| **missing: big** | `jog` and `engine.scratch*` (both wheels); `reverseroll` (shift + play: censor); effect unit 1 (`mix`, `meta`, `enabled`, `focused_effect`, `next_effect`, `prev_effect`, `group_[ChannelN]_enable`, `show_focus`); 16 sampler decks (`[SamplerN] play`, `track_loaded`, `cue_gotoandplay`, `eject`, `[App] num_samplers`) |

Today's hardcoded table (`gui/src/core/midi.rs`) has the same note and CC
numbers as Mixxx's XML for every entry, except three:

- **Tempo slider** (`0xB0 0x00`): Mixxx sends it to a script that joins the MSB
  and LSB (`tempoSliderMSB` / `tempoSliderLSB`).
- **Sync** (`0x90 0x58`): Mixxx sends it to a script (`syncPressed`), which does
  `beatsync` on a short press and toggles sync on a long press.
- **Browse press** (`0x96 0x41`): Mixxx maps it to `MoveFocusForward`, not
  `TogglePane`.

So the "NOT checked against hardware" warning in `midi.rs` can be narrowed to
those three.

## The options

| | A. XML only | B. XML + embedded JS (recommended) | C. Rewrite each controller in Rust | D. Convert to our own format |
|---|---|---|---|---|
| What it is | Read the XML; run direct inputs and outputs; ignore script bindings | A, plus run the scripts in an embedded JS engine with Mixxx's `engine` / `midi` API | Port each mapping's logic by hand into earshot | One-off converter from Mixxx XML (+ hand-ported JS) into a TOML / line format |
| Mappings that load fully | 12 of 159 (8 %) | all 142 MIDI; HID later | only the ones we port | only the ones we port |
| DDJ-400 | mixer, pads, loops, load; no jog, sync, tempo fader, beat jump pads, sampler, FX, VU | everything earshot has controls for | everything, by hand | as C |
| Cost | small: an XML reader and the option maths | medium: JS engine, about 20 API functions, connections, timers, soft takeover, scratch | ~700 lines of JS per controller to port and test, for ever | C plus a converter |
| Follows Mixxx's fixes to mappings | yes | yes | no | no |
| Risk | most controllers half-work | read-after-write (below), JS engine bugs, a misbehaving script | drift from Mixxx; only our hardware works | same as C, and XML-to-format round trips |

**Recommendation: B, built in the order A then B.** A is the first step of B
anyway, and it already gives the phase 0 log (see [README.md](README.md)). C is
right only for earshot-specific behaviour that has no Mixxx equivalent, such as
"browse knob moves the suggestions list". That belongs in our own format, not in
a fork of someone's mapping. D throws away the main benefit: hundreds of mappings
that other people maintain.

### Which JS engine

| | `rquickjs` (QuickJS) | `boa_engine` | `deno_core` (V8) |
|---|---|---|---|
| Language support | ES2020+; enough for every script here (classes, arrows, `let`, string `eval`) | most of ES2015+ | everything |
| Speed | interpreter; far more than MIDI rates need | slower interpreter | JIT, fastest |
| Size / build | small C library, built with `cc` | pure Rust; long compile | tens of MB; prebuilt V8 download |
| Embedding | mature; Rust closures as JS functions; one runtime per thread | younger API | heavy (ops, snapshots, tokio) |

**Pick `rquickjs`**, as `docs/design.md` already says. A controller sends a few
hundred messages a second at most; an interpreter is plenty. V8's size and build
cost buy nothing here. Boa would avoid a C dependency, but it is less proven on
odd real-world scripts. Mixxx's own engine (Qt V4) is also an interpreter with a
JIT for hot code, so QuickJS behaves like what these scripts were written for.

## The translation layer

### Where it sits

```
midir input ──► controller thread ──► Lane (engine commands, no player lock)
 (callback)      │  XML direct inputs + option maths        └► Event::Command (app commands, UI thread)
                 │  QuickJS: script bindings, timers, connections
                 │  control table: [Group] key → cell
                 └► midir output (LEDs) ◄── change notifications (feedback.md)
```

- **One controller thread per device**, like Mixxx. It owns the JS runtime, the
  device's MIDI output, its timers and its connections.
- **The midir callback just forwards bytes** to that thread. Today's
  `on_message` (`gui/src/core/midi.rs`) does the work inside the callback
  instead.
- **Commands leave by the existing routes.** Engine commands go through `Lane`;
  app commands go as `Event::Command`. So nothing here takes the player lock or
  touches the audio thread.

### The control table

Every `[Group] key` a mapping touches resolves, once, to a *cell*:

- **Mapped**: a registry `Control` (`bus.rs`) plus a unit conversion. `get` reads
  the snapshot; `set` sends the command. Examples: `play`, `rate`, EQ.
- **Derived**: a read-only value worked out from the snapshot or app state.
  Examples: `play_indicator`, `beat_active`, `track_loaded`, `duration`,
  `[App] num_decks`.
- **Alias**: another name for a cell. Examples: `filterLow` →
  `[EqualizerRack1_[ChannelN]_Effect1] parameter1`; `reloop_exit` →
  `reloop_toggle`; `hotcue_N_enabled` → `hotcue_N_status`; and the pot-meter
  suffixes (`volume_set_zero`, `rate_perm_up`, …).
- **Stub**: anything else.

A stub works like this:

- It holds a number, starting at 0, or at the mapping's default if one is known.
- `set` stores the number. It also fires the stub's connections, so a script's
  own state machine still works, such as a shift layer kept in a control.
- It logs once per name per session, for example `[Channel1] slip_enabled: not in
  earshot (set by PioneerDDJ400.foo)`.
- It never fails.

Unknown groups (`[Sampler7]`, `[Channel5]`) become stubs in the same way. A JS
exception inside a callback is caught and logged with the function's name. The
controller carries on.

This table is what `Control::mixxx` / `from_mixxx` grows into. The "Equivalent"
column in [mixxx-controls.md](mixxx-controls.md) is its to-do list.

### Things that bite

- **Read-after-write.** In Mixxx, `setValue` changes the control at once, so the
  next `getValue` sees the new value. Scripts rely on this, for example
  `setValue(g, "enabled", !getValue(g, "enabled"))`, or the DDJ-400's
  `changeFocusedEffectBy`. In earshot, a set becomes a command, and the snapshot
  only changes after the next audio callback.

  Fix: the cell keeps a *shadow* of the last value set. `get` returns the shadow
  until a newer snapshot agrees, or until about 100 ms has passed.
- **Set vs. press.** A script's `setValue(play, 1)` means "play". A MIDI press on
  an XML-bound `play` means "toggle". Use `Set::On` / `Set::Off` for the first and
  `Set::Toggle` for the second (see [mixxx-controls.md](mixxx-controls.md)).
- **Units.** Positions are in samples; `super1` runs 0..1; `rateRange` is a
  fraction. Convert in the cell, never in the engine.
- **Curves.** `setParameter` and XML inputs work in knob travel (0..1). Mixxx's
  curve for each control type (log, audio taper, linear) decides what that means.
  Copy its curves where they matter (EQ, volume) so controllers feel the same.
- **Deck counts.** Many scripts read `[App] num_decks` and loop over
  `[ChannelN]`. Return the live deck count. Four-deck controllers' decks 3 and 4
  map to earshot's C and D.
- **Samplers.** Some scripts *write* `[App] num_samplers`, to ask for 16 sampler
  decks. Until earshot has samplers, accept the write into a stub. `[SamplerN]`
  groups then all stub.

## Our own format

Mixxx files stay as they are: read-only inputs, read from
`/usr/share/mixxx/controllers` or wherever the user keeps them. **Don't import or
convert them.** Converting would freeze a copy that stops getting Mixxx's fixes,
and it can't carry the JS anyway.

earshot's own files are for three things:

1. Choosing which Mixxx mapping a device uses.
2. Overriding single bindings on top of it.
3. Binding earshot-only controls: suggestions, Flow, undo, Prepare.

Use the same style as `gui/src/core/keymap.txt`: one binding per line, `#`
comments, and earshot's own control names. It already has a parser, error
reporting, and a Settings page (Settings > Keymap). Its names come from the
registry, so the key help, keymap and controllers all stay in sync. For example,
`~/.local/share/earshot/controllers/ddj-400.txt`:

```
# earshot controller map. Lines: in <status> <data1> <target> <control> [<number>] [options]
#                                out <target> <control> <status> <data1> [on=7F off=00]
device  "DDJ-400"                          # matches the MIDI port name
mixxx   "Pioneer-DDJ-400.midi.xml"         # base mapping: its XML + JS, as is

# Overrides, applied after the Mixxx mapping:
in   0x96 0x41  library  toggle_pane       # browse press: library <-> suggestions
in   0x96 0x42  library  search            # shift + browse press
in   0x90 0x58  deck1    sync              # plain toggle instead of the script
in   0x91 0x58  deck2    sync
out  deck1 quantize  0x90 0x14             # (made-up example) an LED for a control of our own choosing
```

- **A controller with no Mixxx mapping** is just the `in` / `out` lines, with no
  `mixxx` line.
- **If a line format gets cramped,** for example with per-binding curves or
  layers, switch to TOML with the same fields. Either way, it's never XML.
- **Logic that a line can't express** (shift layers, long press) can go in a JS
  file using the same `engine` / `midi` API. We're implementing that API anyway,
  so there's no second scripting system.

Bundling Mixxx's mapping files with earshot would bring GPL-2.0+ obligations.
Reading the ones the user already has installed does not.
