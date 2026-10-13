# Controllers

**Summary:** earshot can run Mixxx's controller mappings unchanged. It would read
Mixxx's XML and JS at runtime, run the JS in an embedded QuickJS, and pass every
`[Group] key` through a translation layer onto earshot's controls. Names earshot
doesn't have yet become harmless stubs that log once. Start with the logging, so
you can plug in a controller, press everything, and read what's missing.

## The docs

- **[mixxx-controls.md](mixxx-controls.md)**: every Mixxx control, what earshot
  has, and how many of Mixxx's 159 mappings use each one. Also the 50 that matter
  most and what the missing ones need.
- **[mixxx-mappings.md](mixxx-mappings.md)**: how Mixxx mappings work (XML, JS,
  the API they call, measured), the options for loading them, the JS engine
  choice, the translation layer, and our own (non-XML) format.
- **[feedback.md](feedback.md)**: LEDs, meters and watching controls. How Mixxx
  does it, how earshot can, how hard it is, and a plan.
- **`tools/`**: the two scripts that measured all this. Rerun them after a Mixxx
  update (see the top of mixxx-controls.md). They read `~/git/mixxx` and
  `/usr/share/mixxx/controllers` and write only the tables in mixxx-controls.md.

## The numbers that matter

- **earshot has 34 of Mixxx's 375 current controls**, but they're the busy ones.
  They cover 36 % of all the control uses across the 159 mappings: `play`, `cue`,
  `rate`, `volume`, EQ, filter, `pfl`, hot cues, loops, beat jump, load,
  crossfader and headphones.
- **Another 130 controls are things earshot already does under another name**:
  `loop_enabled`, `track_loaded`, `vu_meter`, `cue_gotoandplay`, `filterLow`, and
  so on. Wiring up those names lifts coverage to 69 % of uses.
- **Two thirds of mapping inputs go to JS functions**, not straight to controls.
  147 of 159 mappings ship a script. Without a JS engine, 12 mappings (8 %) load
  fully.
- **The biggest missing pieces**, by how many mappings use them:
  - small features: `beatsync` 85, `back`/`fwd` 65, `eject` 49
  - effect units: `mix` 55, `meta` 40 and more
  - jog wheels: `jog` 117; `engine.scratch*` 100
  - slip and reverse: `reverse` 71, `slip_enabled` 38
  - LEDs worked out in the app: `play_indicator` 70, `cue_indicator` 65
  - library navigation
- **137 of 159 mappings send feedback** to the device. Nearly all of what they
  watch is already in earshot's engine snapshot.

## Recommendation

1. **Read Mixxx's files directly, at runtime. Don't convert them.**
   - Converted copies stop getting Mixxx's fixes.
   - They can't carry the JS.
   - There are hundreds of mappings other people maintain; that's the whole
     point.
2. **Embed QuickJS (`rquickjs`)** and implement Mixxx's small script API on top
   of a control table.
   - The API is about 20 `engine.*` functions, plus `midi.sendShortMsg` /
     `sendSysexMsg`.
   - Mixxx's shared JS libraries (`script.*`, `components`) then load as they
     are.
   - `docs/design.md` already chose this.
3. **A translation layer, one cell per `[Group] key`.**
   - Each cell is one of: mapped to a registry `Control` (with unit conversion);
     derived from the snapshot; an alias; or a stub.
   - Stubs keep values, fire connections, log once, and never fail.
   - Shadow values make read-after-write behave as in Mixxx.
4. **Feedback on the controller thread.** About 100 times a second, read
   `Monitor::snapshot()` plus a small published app state. Send what changed.
   The audio thread and the UI aren't involved.
5. **Our own format is keymap.txt-style lines, never XML.** One file per device.
   It names the Mixxx mapping to use, overrides single bindings, and binds
   earshot-only controls such as suggestions, Flow and undo. TOML if it outgrows
   lines.

## Roadmap

Each phase is usable on its own. The percentages are from the stage table in
[mixxx-controls.md](mixxx-controls.md): the share of each mapping's controls
earshot would have.

0. **Log everything; stub everything.**
   - Load the device's Mixxx XML and look up every incoming message in it.
   - Log one line per message: the bytes; the binding's `<description>` from the
     XML; the `[Group] key` or script function; and what earshot did with it.
   - "What earshot did" is one of: **sent** `deck1 play`; **stub**
     `[Channel1] slip_enabled` (missing: slip); **needs JS**
     `PioneerDDJ400.jogTurn`; or **unbound**.
   - At the end, write a summary: bindings touched, how many work, and what's
     missing.
   - This is the "plug it in, press every button, read the log" step. It needs
     only an XML reader and the control table. Today's MIDI monitor
     (`Event::Midi`) is the place to show it.
1. **XML inputs and outputs through the translation layer.**
   - Do the option maths (14-bit, relative, invert, soft takeover).
   - Add the names and aliases for what earshot already does.
   - Add XML `<output>`s for LEDs.
   - Replace the hardcoded DDJ-400 table in `gui/src/core/midi.rs`.
   - Result: 12 XML-only mappings work fully. A DDJ-400 gets its mixer, pads,
     loops, load and button LEDs.
2. **JS.**
   - `rquickjs` on a controller thread per device.
   - The `engine.*` and `midi.*` API, timers, connections, `getSetting`, and
     soft takeover.
   - Stubs for the rest.
   - Result: all 142 MIDI mappings load. With phase 1's names, median coverage
     is 68 % (36 % today).
3. **Small features and app-side LEDs.**
   - `beatsync`, `back`/`fwd`, `eject`, `start`/`stop`, `cue_goto*`, the
     `hotcue_N_goto*` family, `loop_move`, beat jump sizes, `bpm_tap`.
   - `play_indicator`, `cue_indicator`, `beat_active`.
   - Library list navigation.
   - Result: median coverage 85 %.
4. **Jog wheels and slip.**
   - An engine scratch mode behind `engine.scratchEnable` / `scratchTick` /
     `brake` / `spinback`, plus `jog`.
   - Slip, `reverse`, rolls.
   - Result: 40 % of mappings fully covered; median 94 %.
5. **Bigger engine features, as wanted.** Effect units (the biggest by use),
   sampler decks (the DDJ-400's sampler pad mode), key shift and key sync, stereo
   meters, EQ kills.
6. **HID** (16 mappings: Traktor S2/S4, Hercules, …): `hidapi` plus Mixxx's
   `controller.*` HID API.

**The gotcha:** "has the control" isn't enough. Mixxx's scripts assume that
`setValue` takes effect immediately, that positions are in samples, and that
`setValue(play, 1)` means "play", not "toggle". The translation layer has to get
those right, or mappings will half-work in confusing ways (see
[mixxx-mappings.md](mixxx-mappings.md#things-that-bite)).
