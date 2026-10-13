# Mixxx controls: what earshot has and what's missing

**Summary:** earshot answers to 34 of Mixxx's 375 current controls, but those 34
are the ones controllers use most: they make up 36 % of everything the 159 Mixxx
mappings touch. Another third is stuff earshot already does under a different
name. The rest is mostly jog wheels, effects, slip/reverse, and a long tail of
small features.

The numbers:

- **Mixxx documents 421 controls**: 338 you can set, 37 read-only, and 46
  deprecated aliases that old mappings still use. That's 375 current ones.
  (A naive grep of `src/` finds ~550 names, but most are preference keys, not
  controls.)
- **earshot has 34 of them** (9 %) through `Control::from_mixxx`
  (`gui/src/core/bus.rs:790`).
- **130 more are "name only" or "partial"**. earshot can already do them, or
  hold the value, but the Mixxx name isn't wired up. Examples: `loop_enabled`,
  `track_loaded`, `vu_meter`, `cue_gotoandplay`, `filterLow`.
  Adding these names and aliases takes coverage to 46 %. Weighted by what
  mappings actually use, it goes from 36 % to 69 %.
- **196 are missing.** Here's what they need, ranked by how many mappings would
  benefit:
  1. Small features (57 controls): `beatsync`, `back`/`fwd`, `eject`, `bpm_tap`, …
  2. Effect units (35)
  3. Slip, reverse and rolls (6)
  4. Library navigation (18)
  5. LED outputs worked out in the app (5): `play_indicator`, `cue_indicator`,
     `beat_active`
  6. Jog wheels (6): `jog` alone is used by 117 mappings
  7. Then key shift, Auto DJ, samplers, vinyl, inputs, stems, recording

The tables below are generated. To refresh them after a Mixxx update or an
earshot change, edit `STATUS` in `tools/mixxx_tables.py`, then run:

```
cd docs/controllers/tools
python3 -I mixxx_usage.py --out /tmp/usage.json      # reads ~/git/mixxx and /usr/share/mixxx
python3 -I mixxx_tables.py /tmp/usage.json ../mixxx-controls.md
```

## How this was measured

- **The control list** comes from `~/git/mixxx/res/controllers/mixxx-controls.d.ts`
  (Mixxx `main`, `f0d1dd38d5`). It's generated from the manual's controls
  appendix and gives each control's groups, range, and whether it's read-only or
  deprecated. Grepping `src/` for `ConfigKey(…, "name")` found about 20 more
  undocumented internal names (`beat_next`, `rate_ratio`, …). No mapping uses
  them.
- **The mappings** are the 159 mapping files installed with Mixxx 2.5.6 in
  `/usr/share/mixxx/controllers`: 142 MIDI, 16 HID and 1 "bulk". With their
  scripts and shared libraries, that's the 315 files. One mapping means one
  XML file plus the scripts it lists.
- **"Used by"** counts the mappings that use a control in any of three ways:
  - **XML in**: a direct `<control>` input binding.
  - **XML out**: an `<output>` LED binding.
  - **JS**: the name appears in the mapping's own script as a string, including
    names built up like `"hotcue_" + i + "_activate"` or `` `beatloop_${n}_toggle` ``.
    Mappings built on the `components` library are also credited with the
    controls each class drives: a `components.PlayButton` counts as `play` and
    `play_indicator`.
- **The JS count is approximate.** Matching names in script text can't see the
  group, so a generic name like `enabled` can be over-counted. It also misses
  names built entirely from variables. The XML counts are exact.
- Shared libraries (`midi-components-0.0.js`, `lodash.mixxx.js`,
  `common-hid-packet-parser.js`, `common-controller-scripts.js`) aren't counted
  as part of any one mapping.
- **Every XML key matches a documented control**, apart from 14 old names like
  `[Flanger] lfoDepth` that Mixxx itself no longer has. So the control list is
  complete for what mappings use.

## Things to know before wiring names up

These are where "the same control" means something different in Mixxx and in
earshot. The translation layer (see [mixxx-mappings.md](mixxx-mappings.md)) has
to handle each one.

- **Setting from a script is not pressing a button.** When a MIDI note arrives
  for `[Channel1] play` (XML input), Mixxx toggles it on press, because `play` is
  a toggle button (`ControlPushButtonBehavior::setValueFromMidi`,
  `src/control/controlbehavior.cpp:351`). But `engine.setValue("[Channel1]",
  "play", 1)` from a script *sets* it to 1. Today `DeckControl::command` turns any
  value > 0 into `Set::Toggle` (`bus.rs:502`), so a script's `setValue(…, 1)` on a
  playing deck would pause it. The script path needs `Set::On` / `Set::Off`.
- **Positions are in samples, not seconds.** `cue_point`, `hotcue_N_position`,
  `loop_start_position` and `loop_end_position` count interleaved stereo samples
  (frames × 2) in Mixxx. earshot uses seconds. Convert with the track's sample
  rate. The DDJ-400's loop-adjust jog writes `loop_start_position` in these units
  (`Pioneer-DDJ-400-script.js`, `jogTurn`).
- **`rateRange` is a value, not a button.** In Mixxx it's the range as a fraction:
  0.08 means ±8 %. The DDJ-400 script cycles it through 0.06, 0.10, 0.16, 0.25.
  earshot maps it to "next range" (`bus.rs:570`). It should snap the value to the
  nearest `TempoRange` instead (±6, ±10, ±16, WIDE).
- **Knob curves differ.** For a 7-bit MIDI value, Mixxx uses a curve that depends
  on the control:
  - log for EQ and gain, with 1.0 at the centre;
  - audio taper for `volume`;
  - linear for `rate`, `crossfader`, `super1`.

  earshot's `Range::from_pos` is linear on either side of the centre. The ends
  and the centre agree, but values in between don't, so a knob will feel slightly
  different from Mixxx. `super1` runs 0..1 in Mixxx and −1..1 in earshot.
- **Aliases matter.** Old names still in heavy use:
  - `filterLow` / `filterMid` / `filterHigh` on `[ChannelN]`: 58 mappings. Mixxx
    maps them to the EQ rack's `parameter1-3`
    (`src/effects/chains/equalizereffectchain.cpp:54`).
  - `reloop_exit`: 77 mappings. It's `reloop_toggle`.
  - `hotcue_N_enabled`: 69 mappings, including the DDJ-400's pad LEDs. It's
    `hotcue_N_status`.
  - `[Master] num_decks` etc.: now `[App]`.

  Mixxx's aliases are listed by `grep -rn addAlias src/`.
- **Pot-meter suffixes.** Every knob control also takes `_up`, `_down`,
  `_up_small`, `_down_small`, `_set_default`, `_set_zero`, `_set_one`,
  `_set_minus_one`, `_toggle` and `_minus_toggle`. One rule covers them all.
- **Deck numbering.** Mixxx's `[Channel3]` and `[Channel4]` are earshot's decks C
  and D. `[PreviewDeck1]` is Prepare's player today (`MixxxGroup::parse`). Mixxx's
  preview deck is a headphone-only player, so earshot's library preview player
  (`PreviewCmd`) may be the closer match.
- **Not Mixxx names.** earshot's `[Library] TogglePane`, `Undo` and `Redo` are its
  own inventions. That's fine; they just won't appear in any Mixxx mapping.

## The tables

<!-- BEGIN GENERATED -->
### Coverage by group

Current controls only (deprecated aliases are listed in the tables but not counted here). *Coverage* is **have** ÷ (all − n/a); *with names* adds **name** and **partial**: what a translation layer gets without new engine work.

| Group | Controls | have | partial | name | missing | n/a | Coverage | With names |
|---|--:|--:|--:|--:|--:|--:|--:|--:|
| Decks | 215 | 28 | 6 | 78 | 102 | 1 | 13% | 52% |
| Mixer | 26 | 5 | 0 | 15 | 6 | 0 | 19% | 77% |
| [App] | 13 | 0 | 0 | 12 | 1 | 0 | 0% | 92% |
| Library | 41 | 1 | 4 | 13 | 20 | 3 | 3% | 47% |
| Effects | 39 | 0 | 2 | 0 | 37 | 0 | 0% | 5% |
| Inputs | 17 | 0 | 0 | 0 | 17 | 0 | 0% | 0% |
| [AutoDJ] | 5 | 0 | 0 | 0 | 5 | 0 | 0% | 0% |
| [Sampler] (banks) | 2 | 0 | 0 | 0 | 2 | 0 | 0% | 0% |
| [Recording] | 2 | 0 | 0 | 0 | 2 | 0 | 0% | 0% |
| [VinylControl] | 2 | 0 | 0 | 0 | 2 | 0 | 0% | 0% |
| Stems | 1 | 0 | 0 | 0 | 1 | 0 | 0% | 0% |
| [Waveform] | 1 | 0 | 0 | 0 | 1 | 0 | 0% | 0% |
| [Skin] | 6 | 0 | 0 | 0 | 0 | 6 | – | – |
| [Controls] | 3 | 0 | 0 | 0 | 0 | 3 | – | – |
| [Shoutcast] | 2 | 0 | 0 | 0 | 0 | 2 | – | – |
| **All** | **375** | **34** | **12** | **118** | **196** | **15** | **9%** | **46%** |

### Weighted by what mappings use

Counting each (mapping, control) pair once across all 159 mappings: 6587 uses.

| Status | Uses | Share |
|---|--:|--:|
| have | 2400 | 36% |
| partial | 329 | 5% |
| name | 1829 | 28% |
| missing | 1979 | 30% |
| n/a | 50 | 1% |

### The 50 controls mappings use most

*Used by* is how many of the 159 mappings use the control at all; *XML in*, *XML out* and *JS* split that by where it appears (see the method notes).

| # | Control | Group | Used by | XML in | XML out | JS | earshot | Equivalent / needs |
|--:|---|---|--:|--:|--:|--:|---|---|
| 1 | `play` | [ChannelN] | 149 | 69 | 24 | 117 | have | Play |
| 2 | `rate` | [ChannelN] | 139 | 59 | 6 | 95 | have | Tempo (TempoFader) |
| 3 | `cue_default` | [ChannelN] | 136 | 62 | 16 | 85 | have | Cue (CDJ style) |
| 4 | `volume` | [ChannelN] | 132 | 72 | 0 | 72 | have | Volume |
| 5 | `pfl` | [ChannelN] | 126 | 59 | 46 | 77 | have | Pfl |
| 6 | `crossfader` | [Master] | 125 | 87 | 0 | 47 | have | Master(Crossfader) |
| 7 | `LoadSelectedTrack` | [ChannelN] | 124 | 48 | 0 | 91 | have | LoadSelected |
| 8 | `pregain` | [ChannelN] | 118 | 58 | 0 | 69 | have | Trim |
| 9 | `jog` | [ChannelN] | 117 | 5 | 1 | 113 | missing | *jog wheels and scratching* |
| 10 | `hotcue_N_activate` | [ChannelN] | 101 | 27 | 0 | 77 | have | Hotcue(n) |
| 11 | `loop_out` | [ChannelN] | 94 | 27 | 4 | 71 | have | LoopOut |
| 12 | `loop_in` | [ChannelN] | 93 | 27 | 4 | 70 | have | LoopIn |
| 13 | `hotcue_N_clear` | [ChannelN] | 90 | 21 | 0 | 70 | have | HotcueClear(n) |
| 14 | `headMix` | [Master] | 88 | 52 | 0 | 43 | have | Master(HeadMix) |
| 15 | `loop_enabled` | [ChannelN] | 87 | 2 | 20 | 71 | name | DeckSnap.loop_active |
| 16 | `beatsync` | [ChannelN] | 85 | 36 | 9 | 52 | missing | (Sync exists); *a small engine or app feature* |
| 17 | `playposition` | [ChannelN] | 85 | 7 | 9 | 73 | have | Position |
| 18 | `keylock` | [ChannelN] | 83 | 33 | 19 | 56 | have | Keylock |
| 19 | `reloop_exit` | [ChannelN] | 77 | 26 | 1 | 53 | name | Reloop (alias of reloop_toggle) |
| 20 | `parameterN` | [EqualizerRack1_[ChannelI]_Effect1] | 72 | 27 | 0 | 52 | partial | Eq(band) (have for the EQ rack's parameter1-3; missing for effect units) |
| 21 | `super1` | [QuickEffectRack1_[ChannelI]] | 72 | 17 | 0 | 60 | partial | Filter (have for [QuickEffectRack1_[ChannelN]] (the filter); missing for effect units) |
| 22 | `reverse` | [ChannelN] | 71 | 20 | 2 | 56 | missing | *slip, reverse and rolls* |
| 23 | `play_indicator` | [ChannelN] | 70 | 0 | 25 | 46 | missing | (DeckSnap.playing); *an LED output worked out in the app* |
| 24 | `loop_halve` | [ChannelN] | 69 | 22 | 5 | 52 | have | LoopHalve |
| 25 | `quantize` | [ChannelN] | 69 | 14 | 7 | 59 | have | Quantize |
| 26 | `hotcue_N_enabled` | [ChannelN] | 69 | 0 | 23 | 51 | name | DeckControl::Hotcue get (alias of hotcue_N_status) |
| 27 | `loop_double` | [ChannelN] | 68 | 21 | 5 | 52 | have | LoopDouble |
| 28 | `sync_enabled` | [ChannelN] | 65 | 23 | 20 | 46 | have | Sync |
| 29 | `back` | [ChannelN] | 65 | 34 | 2 | 34 | missing | (Seek exists); *a small engine or app feature* |
| 30 | `fwd` | [ChannelN] | 65 | 34 | 2 | 34 | missing | (Seek exists); *a small engine or app feature* |
| 31 | `cue_indicator` | [ChannelN] | 65 | 0 | 25 | 41 | missing | (DeckSnap.playing, cue_point); *an LED output worked out in the app* |
| 32 | `rate_temp_down` | [ChannelN] | 63 | 34 | 5 | 33 | have | NudgeDown (earshot's nudge is a fixed 4 %) |
| 33 | `rate_temp_up` | [ChannelN] | 63 | 33 | 5 | 33 | have | NudgeUp |
| 34 | `enabled` | [EffectRack1_EffectUnitN_EffectM] | 61 | 17 | 17 | 48 | missing | *effect units* |
| 35 | `filterMid` | [ChannelN] | 59 | 42 | 0 | 22 | name | Eq(EQ_MID) (alias of the EQ rack's parameter2) |
| 36 | `filterHigh` | [ChannelN] | 58 | 42 | 0 | 22 | name | Eq(EQ_HIGH) (alias of the EQ rack's parameter3) |
| 37 | `filterLow` | [ChannelN] | 58 | 42 | 0 | 22 | name | Eq(EQ_LOW) (alias of the EQ rack's parameter1) |
| 38 | `SelectNextTrack` | [Playlist] | 57 | 21 | 0 | 40 | name | Browse(1) |
| 39 | `SelectPrevTrack` | [Playlist] | 57 | 21 | 0 | 40 | name | Browse(-1) |
| 40 | `mix` | [EffectRack1_EffectUnitN] | 55 | 5 | 0 | 50 | missing | *effect units* |
| 41 | `cue_gotoandplay` | [SamplerN] | 54 | 16 | 1 | 39 | name | Seek + Play |
| 42 | `SelectNextPlaylist` | [Playlist] | 53 | 12 | 0 | 43 | missing | (Source exists); *library or window navigation* |
| 43 | `SelectPrevPlaylist` | [Playlist] | 51 | 11 | 0 | 42 | missing | (Source exists); *library or window navigation* |
| 44 | `SelectTrackKnob` | [Playlist] | 50 | 9 | 0 | 41 | name | Browse |
| 45 | `eject` | [SamplerN] | 49 | 9 | 0 | 42 | missing | *a small engine or app feature* |
| 46 | `loop_end_position` | [ChannelN] | 47 | 0 | 4 | 44 | name | DeckSnap.loop_out / SetLoop (samples vs seconds) |
| 47 | `duration` | [ChannelN] | 46 | 0 | 1 | 45 | name | DeckSnap.duration |
| 48 | `num_samplers` | [App] | 46 | 0 | 0 | 46 | missing | *sampler decks* |
| 49 | `vu_meter` | [ChannelN] | 45 | 0 | 7 | 38 | name | DeckSnap.vu |
| 50 | `loop_start_position` | [ChannelN] | 45 | 0 | 5 | 41 | name | DeckSnap.loop_in / SetLoop (samples vs seconds) |

### What the missing controls need

Current controls with status **missing** (and **partial** ones that still need work), by the work they need. *Uses* sums how many mappings use each one.

| Needs | Controls | Uses | Most used |
|---|--:|--:|---|
| a small engine or app feature | 57 | 546 | `beatsync` (85), `back` (65), `fwd` (65), `eject` (49), `bpm_tap` (28) |
| effect units | 35 | 446 | `enabled` (61), `mix` (55), `group_[ChannelN]_enable` (40), `meta` (40), `focused_effect` (27) |
| slip, reverse and rolls | 6 | 190 | `reverse` (71), `reverseroll` (43), `slip_enabled` (38), `beatlooproll_N_activate` (25), `beatlooproll_activate` (13) |
| library or window navigation | 18 | 185 | `SelectNextPlaylist` (53), `SelectPrevPlaylist` (51), `GoToItem` (23), `ToggleSelectedSidebarItem` (20), `MoveHorizontal` (6) |
| an LED output worked out in the app | 5 | 176 | `play_indicator` (70), `cue_indicator` (65), `beat_active` (34), `beat_closest` (4), `beat_distance` (3) |
| jog wheels and scratching | 6 | 150 | `jog` (117), `scratch2_enable` (13), `scratch2` (11), `wheel` (9), `scratch_position` (0) |
| key shift and key sync | 8 | 91 | `pitch` (23), `pitch_adjust` (16), `reset_key` (13), `sync_key` (12), `pitch_down` (8) |
| sampler decks | 5 | 46 | `num_samplers` (46), `CloneFromSampler` (0), `LoadTrackFromSampler` (0), `LoadSamplerBank` (0), `SaveSamplerBank` (0) |
| timecode vinyl | 6 | 30 | `passthrough` (9), `vinylcontrol_enabled` (7), `vinylcontrol_mode` (6), `vinylcontrol_cueing` (4), `vinylcontrol_status` (4) |
| recording | 2 | 21 | `status` (11), `toggle_recording` (10) |
| Auto DJ | 6 | 19 | `AutoDjAddBottom` (5), `AutoDjAddTop` (5), `skip_next` (5), `fade_now` (3), `shuffle_playlist` (1) |
| intro / outro markers | 20 | 12 | `intro_end_activate` (2), `intro_start_activate` (2), `outro_end_activate` (2), `outro_start_activate` (2), `intro_end_enabled` (1) |
| microphone and aux inputs | 8 | 10 | `talkover` (8), `main_mix` (1), `input_configured` (1), `duckStrength` (0), `talkoverDucking` (0) |
| stems | 3 | 1 | `color` (1), `stem_count` (0), `stem_split_tracks` (0) |

### How many mappings would be fully covered

For each mapping, the share of the controls it uses that earshot would have at each stage (n/a controls count as covered: a mapping touching them just gets a no-op). This is about control names only; running the mapping's JS is a separate requirement (see mixxx-mappings.md).

| Stage | Mappings 100 % covered | ≥ 90 % | ≥ 75 % | Median coverage |
|---|--:|--:|--:|--:|
| Today (Control::from_mixxx) | 3 (2%) | 3 (2%) | 3 (2%) | 36% |
| + names and aliases for what earshot already does | 7 (4%) | 11 (7%) | 47 (29%) | 68% |
| + small features, LED outputs, library navigation | 16 (10%) | 58 (35%) | 118 (72%) | 85% |
| + jog / scratch and slip / reverse | 66 (40%) | 98 (60%) | 138 (84%) | 94% |

## Every control, by group

*Type* comes from the manual's range: binary (a button or switch), a numeric range (a knob or fader), relative (an encoder), enum, or read-only (an output for LEDs and scripts). Names with `N` stand for a family (`hotcue_N_activate` is `hotcue_1_activate` … `hotcue_36_activate`). Pot-meter controls also take the `_up`, `_down`, `_set_default`, `_set_zero`, `_toggle` … suffixes (marked ±). *Used by* counts XML uses in this table's groups, but JS uses by name only, so a name that also exists in another group (marked †) may count the other group's JS uses too.

### Decks: [ChannelN], [PreviewDeckN], [SamplerN] (and stem sub-decks)

Groups: `[ChannelN]`, `[ChannelN_StemM]`, `[PreviewDeckN]`, `[SamplerN]`.

| Control | What it does | Type | earshot | Equivalent / needs | Used by (XML in / out / JS) |
|---|---|---|---|---|---|
| `play` | Toggles playing or pausing the track. | binary | **have** | Play | 149 (69 / 24 / 117) |
| `rate` ± | Speed control. | -1.0..1.0 | **have** | Tempo (TempoFader) | 139 (59 / 6 / 95) |
| `cue_default` | In CDJ mode, when playing, returns to the cue point and pauses. | binary | **have** | Cue (CDJ style) | 136 (62 / 16 / 85) |
| `volume` ± † | Adjusts the channel volume fader. | value | **have** | Volume | 130 (68 / 0 / 72) |
| `pfl` † | Toggles headphone cueing (PFL). | binary | **have** | Pfl | 126 (59 / 46 / 77) |
| `LoadSelectedTrack` | Loads the currently highlighted track into the deck | binary | **have** | LoadSelected | 124 (48 / 0 / 91) |
| `pregain` ± † | Adjusts the pre-fader gain of the track (to avoid clipping). | 0.0..1.0..4.0 | **have** | Trim | 118 (57 / 0 / 69) |
| `hotcue_N_activate` | If hotcue X is not set, this sets a hotcue at the current play position and saves it as… | binary | **have** | Hotcue(n) | 101 (27 / 0 / 77) |
| `loop_out` | If loop is disabled, sets the player loop out position to the current play position. | binary | **have** | LoopOut | 94 (27 / 4 / 71) |
| `loop_in` | If loop is disabled, sets the player loop in position to the current play position. | binary | **have** | LoopIn | 93 (27 / 4 / 70) |
| `hotcue_N_clear` | If hotcue X is set, clears its hotcue status. | binary | **have** | HotcueClear(n) | 90 (21 / 0 / 70) |
| `playposition` ± | Sets the absolute position in the track. | value | **have** | Position | 85 (7 / 9 / 73) |
| `keylock` | Enable key-lock for the specified deck (rate changes only affect tempo, not key) | binary | **have** | Keylock | 83 (33 / 19 / 56) |
| `loop_halve` | Halves beatloop_size. | binary | **have** | LoopHalve | 69 (22 / 5 / 52) |
| `quantize` | Aligns Hot-cues and Loop In & Out to the next beat from the current position. | binary | **have** | Quantize | 69 (14 / 7 / 59) |
| `loop_double` | Doubles beatloop_size. | binary | **have** | LoopDouble | 68 (21 / 5 / 52) |
| `sync_enabled` | Syncs the tempo and phase (depending on quantize) to that of the other track (if BPM is… | binary | **have** | Sync | 65 (23 / 20 / 46) |
| `rate_temp_down` | Holds the speed one step lower while active | binary | **have** | NudgeDown; earshot's nudge is a fixed 4 % | 63 (34 / 5 / 33) |
| `rate_temp_up` | Holds the speed one step higher while active | binary | **have** | NudgeUp | 63 (33 / 5 / 33) |
| `beatloop_N_toggle` | Toggles a loop over X beats. | binary | **have** | BeatLoop(n) | 37 (10 / 0 / 27) |
| `reloop_toggle` | Toggles the current loop on or off. | binary | **have** | Reloop | 36 (8 / 0 / 28) |
| `beatloop_activate` | Set a loop that is beatloop_size beats long and enables the loop. | binary | **have** | Loop | 33 (2 / 0 / 31) |
| `beatjump_backward` | Jump backward by beatjump_size. | binary | **have** | JumpBack | 24 (2 / 0 / 22) |
| `beatjump_size` | Set the number of beats to jump with beatjump_forward /beatjump_backward. | positive real number | **have** | JumpSize | 24 (0 / 0 / 24) |
| `beatjump_forward` | Jump forward by beatjump_size. | binary | **have** | JumpForward | 22 (2 / 0 / 20) |
| `cue_set` | Sets a cue point at the current location. | binary | **have** | SetCue | 17 (9 / 0 / 9) |
| `beatjump` | Jump forward (positive) or backward (negative) by N beats. | value | **have** | BeatJump | 12 (0 / 0 / 12) |
| `beatloop_size` | Set the length of the loop in beats that will get set with beatloop_activate and… | positive real number | **have** | LoopSize | 12 (0 / 0 / 12) |
| `rateRange` ± | Sets the range of the Speed slider (0.08 = 8%). | 0.0..4.0 | partial | TempoRangeNext; earshot treats it as a "next range" button; Mixxx's is a value (0.08 = ±8 %). Needs mapping to the nearest TempoRange | 37 (0 / 0 / 37) |
| `hotcue_N_color` | Color of hotcue X or -1 if the hotcue is not set. | colour | partial | memory cues have colours; hot cues only labels; *a small engine or app feature* | 23 (0 / 0 / 23) |
| `vu_meter_left` † | Outputs the current instantaneous deck volume for the left channel (or stem volume for… | value | partial | DeckSnap.vu (mono); *a small engine or app feature* | 20 (0 / 0 / 20) |
| `vu_meter_right` † | Outputs the current instantaneous deck volume for the right channel (or stem volume for… | value | partial | DeckSnap.vu (mono); *a small engine or app feature* | 20 (0 / 0 / 20) |
| `peak_indicator_left` † | Indicates when the signal is clipping (too loud for the hardware and is being distorted)… | binary | partial | DeckSnap.vu (mono); *a small engine or app feature* | 4 (0 / 0 / 4) |
| `peak_indicator_right` † | Indicates when the signal is clipping (too loud for the hardware and is being distorted)… | binary | partial | DeckSnap.vu (mono); *a small engine or app feature* | 4 (0 / 0 / 4) |
| `loop_enabled` | Indicates whether or not a loop is enabled. | binary | name | DeckSnap.loop_active | 87 (2 / 20 / 71) |
| `cue_gotoandplay` | If the cue point is set, seeks the player to it and starts playback. | binary | name | Seek + Play | 54 (16 / 1 / 39) |
| `loop_end_position` | The player loop-out position in samples, -1 if not set. | integer | name | DeckSnap.loop_out / SetLoop; samples vs seconds | 47 (0 / 4 / 44) |
| `duration` | Outputs the length of the current song in seconds | absolute value | name | DeckSnap.duration | 46 (0 / 1 / 45) |
| `loop_start_position` | The player loop-in position in samples, -1 if not set. | integer | name | DeckSnap.loop_in / SetLoop; samples vs seconds | 45 (0 / 5 / 41) |
| `vu_meter` † | Outputs the current instantaneous deck volume (or stem volume for [ChannelN_StemM]) | value | name | DeckSnap.vu | 44 (0 / 6 / 38) |
| `beatloop_N_enabled` | 1 if beatloop X is enabled, 0 if not. | binary | name | DeckSnap loop_active + length | 43 (0 / 10 / 33) |
| `start_stop` | Seeks a player to the start and then stops it. | binary | name | Seek + Play(Off) | 41 (7 / 0 / 34) |
| `beats_translate_curpos` | Adjust beatgrid so closest beat is aligned with the current playposition. | binary | name | GridOp::AnchorHere | 40 (13 / 2 / 27) |
| `beatloop_N_activate` | Activates a loop over X beats. | binary | name | BeatLoop(n); activate always (re)starts; earshot's toggles | 38 (3 / 0 / 35) |
| `track_loaded` | Whether a track is loaded in the specified deck | read-only binary | name | DeckSnap.track_id | 37 (0 / 2 / 37) |
| `bpm` ± | Reflects the perceived (rate-adjusted) BPM of the loaded file. | real-valued | name | DeckSnap.bpm (read); setting it sets the tempo | 32 (1 / 0 / 31) |
| `track_samples` | Number of sound samples in the track loaded on the specified deck | read-only absolute value | name | DeckSnap.duration × rate | 29 (0 / 1 / 28) |
| `peak_indicator` † | Indicates when the signal is clipping (too loud for the hardware and is being distorted) | binary | name | DeckSnap.vu ≥ 1 | 28 (0 / 4 / 24) |
| `hotcue_N_status` | Indicates if hotcue slot X is set, active or empty. | value | name | DeckControl::Hotcue get | 24 (0 / 2 / 23) |
| `stop` | Stops a player. | binary | name | Play(Off) | 24 (7 / 1 / 20) |
| `LoadSelectedTrackAndPlay` | Loads the currently highlighted track into the deck and starts playing. | binary | name | LoadSelected + Play | 23 (4 / 0 / 20) |
| `cue_gotoandstop` | If the cue point is set, seeks the player to it and stops. | binary | name | Seek + Play(Off) | 23 (6 / 0 / 18) |
| `track_samplerate` | Sample rate of the track loaded on the specified deck | read-only absolute value | name | the track's sample rate | 23 (0 / 0 / 23) |
| `hotcue_N_position` | The position of hotcue X in samples, -1 if not set. | integer | name | DeckSnap.hotcues[n]; samples vs seconds | 22 (0 / 0 / 22) |
| `start` | Jump to start of track | binary | name | Seek | 21 (6 / 0 / 15) |
| `start_play` | Start playback from the beginning of the deck. | binary | name | Seek + Play | 20 (10 / 2 / 10) |
| `hotcue_N_gotoandplay` | If hotcue X is set, seeks the player to hotcue X’s position and starts playback. | binary | name | Seek + Play | 19 (0 / 0 / 19) |
| `beatjump_N_backward` | Jump backward by X beats. | binary | name | BeatJump(-n) | 18 (5 / 1 / 14) |
| `beatjump_N_forward` | Jump forward by X beats. | binary | name | BeatJump(n) | 18 (5 / 1 / 14) |
| `file_bpm` | The detected BPM of the loaded track. | read-only positive value | name | the track's BPM | 16 (0 / 0 / 16) |
| `rate_dir` | Indicates orientation of speed slider. | relative | name | a constant (earshot's fader direction is fixed) | 16 (0 / 0 / 16) |
| `cue_goto` | If the cue point is set, recalls the cue point. | binary | name | Seek to DeckSnap.cue_point | 15 (6 / 1 / 9) |
| `orientation` † | Set channel orientation for the crossfader. | enum | name | DeckCmd::Crossfader(XfSide) | 15 (0 / 0 / 15) |
| `rate_perm_up` | Sets the speed one step higher (4 % default) | binary | name | engine Rate | 15 (8 / 0 / 7) |
| `beats_translate_earlier` | Move beatgrid to an earlier position. | binary | name | GridOp::Nudge (grid_nudge) | 14 (0 / 0 / 14) |
| `beats_translate_later` | Move beatgrid to a later position. | binary | name | GridOp::Nudge (grid_nudge) | 14 (0 / 0 / 14) |
| `rate_perm_down` | Sets the speed one step lower (4 % default) lower | binary | name | engine Rate | 14 (7 / 0 / 7) |
| `rate_temp_down_small` | Holds the speed one small step lower while active | binary | name | Nudge | 13 (7 / 1 / 6) |
| `rate_temp_up_small` | Holds the speed one small step higher while active | binary | name | Nudge | 13 (7 / 1 / 6) |
| `beats_adjust_faster` | Adjust the average BPM up by +0.01 | binary | name | GridOp::SetBpm | 11 (1 / 0 / 10) |
| `beats_adjust_slower` | Adjust the average BPM down by -0.01. | binary | name | GridOp::SetBpm | 11 (1 / 0 / 10) |
| `cue_point` | The current position of the cue point in samples. | absolute value | name | DeckSnap.cue_point / SetCue(Some); Mixxx counts samples, earshot seconds | 11 (0 / 2 / 9) |
| `hotcue_N_goto` | If hotcue X is set, seeks the player to hotcue X’s position. | binary | name | Seek to hot cue | 11 (0 / 0 / 11) |
| `end` | Jump to end of track | binary | name | Seek | 10 (3 / 0 / 7) |
| `hotcue_N_gotoandstop` | If hotcue X is set, seeks the player to hotcue X’s position and stops. | binary | name | Seek + Play(Off) | 10 (0 / 0 / 10) |
| `loop_scale` | Scale the loop length by the value scale is set to by moving the end marker. | 0.0 - infinity | name | engine LoopScale | 10 (2 / 0 / 8) |
| `rate_perm_down_small` | Sets the speed one small step lower (1 % default) | binary | name | engine Rate | 10 (3 / 0 / 7) |
| `rate_perm_up_small` | Sets the speed one small step higher (1 % default) | binary | name | engine Rate | 10 (3 / 0 / 7) |
| `sync_leader` | Sets deck as leader clock. | binary | name | SetSyncLeader / MixerSnap.sync_leader | 10 (7 / 5 / 3) |
| `end_of_track` | Switches to 1 if the play position is within the end range defined in Preferences ‣… | read-only binary | name | EngineEvent::TrackEnded / position | 6 (0 / 2 / 5) |
| `sync_mode` | (No description) | enum | name | DeckSnap.sync / following | 6 (0 / 0 / 6) |
| `waveform_zoom` | Zooms the waveform to look ahead or back as needed. | 1.0 - 10.0 | name | ViewCmd::WaveZoom | 6 (0 / 0 / 6) |
| `waveform_zoom_down` | Waveform Zoom In | value | name | ViewCmd::WaveZoom | 6 (0 / 0 / 6) |
| `waveform_zoom_up` | Waveform Zoom Out | value | name | ViewCmd::WaveZoom | 6 (0 / 0 / 6) |
| `play_latched` | This is set to 1 when the track is playing, but not when previewing (see play). | read-only binary | name | DeckSnap.playing | 5 (0 / 0 / 5) |
| `cue_cdj` | Represents a Cue button that is always in CDJ mode. | binary | name | Cue | 4 (0 / 0 / 4) |
| `file_key` | The detected key of the loaded track. | read-only value | name | the track's key | 4 (0 / 0 / 4) |
| `track_color` | Color of the currently loaded track or -1 if no track is loaded or the track has no color. | colour | name | Edit Colour | 4 (0 / 0 / 4) |
| `visual_bpm` | BPM to display in the GUI (updated more slowly than the actual BPM). | value | name | DeckSnap.bpm | 4 (0 / 0 / 4) |
| `visual_key` | Current musical key after pitch shifting to display in the GUI using the notation… | value | name | the track's effective key (UI) | 4 (0 / 0 / 4) |
| `waveform_zoom_set_default` | Return to default waveform zoom level | value | name | ViewCmd::WaveZoom | 4 (0 / 0 / 4) |
| `sync_master` | Sets deck as leader clock. | binary | name | SetSyncLeader; alias of sync_leader | 3 (0 / 0 / 3) |
| `hotcue_N_type` | Indicates the type of the hotcue in hotcue slot X. | value | name | always 1 (cue) | 2 (0 / 0 / 2) |
| `hotcue_N_activatecue` | Identical to hotcue_X_activate, but this always sets a regular cue point, regardless of… | value | name | Hotcue(n) | 1 (0 / 0 / 1) |
| `local_bpm` | Reflects the average bpm around the current play position of the loaded file. | positive value | name | DeckSnap.bpm | 1 (0 / 0 / 1) |
| `stars_down` † | Decrease the rating of the currently loaded track (if the skin has star widgets in the… | binary | name | Edit Rating | 1 (0 / 0 / 1) |
| `stars_up` † | Increase the rating of the currently loaded track (if the skin has star widgets in the… | binary | name | Edit Rating | 1 (0 / 0 / 1) |
| `beatjump_size_double` | Double the value of beatjump_size. | binary | name | SetJumpSize | – |
| `beatjump_size_halve` | Halve the value of beatjump_size. | binary | name | SetJumpSize | – |
| `beats_set_double` | Scale the BPM by 2. | binary | name | GridOp::Double | – |
| `beats_set_fourthirds` | Scale the BPM by 4/3. | binary | name | GridOp::SetBpm | – |
| `beats_set_halve` | Scale the BPM by 1/2. | binary | name | GridOp::Halve | – |
| `beats_set_threefourths` | Scale the BPM by 3/4. | binary | name | GridOp::SetBpm | – |
| `beats_set_threehalves` | Scale the BPM by 3/2. | binary | name | GridOp::SetBpm | – |
| `beats_set_twothirds` | Scale the BPM by 2/3. | binary | name | GridOp::SetBpm | – |
| `beats_undo_adjustment` | Restores the beatgrid state before the last beatgrid adjustment done with the above… | binary | name | Undo | – |
| `rateEngine` | Actual rate (used in visuals, not for control) | value | name | DeckSnap.speed | – |
| `stars_five` | Give the currently loaded track a rating of 5 stars. | binary | name | Edit Rating | – |
| `stars_four` | Give the currently loaded track a rating of 4 stars. | binary | name | Edit Rating | – |
| `stars_one` | Give the currently loaded track a rating of 1 star. | binary | name | Edit Rating | – |
| `stars_three` | Give the currently loaded track a rating of 3 stars. | binary | name | Edit Rating | – |
| `stars_two` | Give the currently loaded track a rating of 2 stars. | binary | name | Edit Rating | – |
| `jog` | Affects relative playback speed and direction for short instances (additive & is… | -3.0..3.0 | missing | *jog wheels and scratching* | 117 (5 / 1 / 113) |
| `beatsync` | Syncs the tempo and phase (depending on quantize) to that of the other track (if BPM is… | value | missing | (Sync exists); *a small engine or app feature* | 85 (36 / 9 / 52) |
| `reverse` | Toggles playing the track backwards | binary | missing | *slip, reverse and rolls* | 71 (20 / 2 / 56) |
| `play_indicator` | Provides information to be bound with the a Play/Pause button e.g blinking when play is… | read-only binary | missing | (DeckSnap.playing); *an LED output worked out in the app* | 70 (0 / 25 / 46) |
| `back` | Fast rewind (REW) | binary | missing | (Seek exists); *a small engine or app feature* | 65 (34 / 2 / 34) |
| `cue_indicator` | Indicates the blinking pattern of the CUE button (i.e. | binary | missing | (DeckSnap.playing, cue_point); *an LED output worked out in the app* | 65 (0 / 25 / 41) |
| `fwd` | Fast forward (FF) | binary | missing | (Seek exists); *a small engine or app feature* | 65 (34 / 2 / 34) |
| `eject` | Eject currently loaded track. | binary | missing | *a small engine or app feature* | 49 (9 / 0 / 42) |
| `reverseroll` | Enables reverse and slip mode while held (Censor) | binary | missing | *slip, reverse and rolls* | 43 (4 / 1 / 39) |
| `slip_enabled` | Toggles slip mode. | binary | missing | *slip, reverse and rolls* | 38 (5 / 6 / 37) |
| `beat_active` | Indicates, depending on the play direction, how the player is currently positioned to… | read-only real number | missing | (grid in DeckSession); *an LED output worked out in the app* | 34 (0 / 6 / 29) |
| `bpm_tap` | When tapped repeatedly, adjusts the BPM of the track on the deck (not the tempo slider!)… | binary | missing | *a small engine or app feature* | 28 (11 / 1 / 17) |
| `repeat` | Enable repeat-mode for the specified deck | binary | missing | *a small engine or app feature* | 26 (3 / 2 / 24) |
| `beatlooproll_N_activate` | Activates a rolling loop over X beats. | binary | missing | *slip, reverse and rolls* | 25 (3 / 2 / 22) |
| `pitch` ± | The total adjustment to the track’s pitch, including changes from the rate slider if… | -6.0..6.0 semitones | missing | *key shift and key sync* | 23 (2 / 1 / 21) |
| `reloop_andstop` | Activate current loop, jump to its loop in point, and stop playback. | binary | missing | *a small engine or app feature* | 21 (3 / 0 / 18) |
| `play_stutter` | A play button without pause. | binary | missing | *a small engine or app feature* | 17 (8 / 1 / 9) |
| `pitch_adjust` ± | Adjusts the pitch in addition to the tempo slider pitch and keylock. | -3.0..3.0 semitones | missing | *key shift and key sync* | 16 (1 / 0 / 15) |
| `hotcue_N_set` | Set a hotcue at the current play position and saves it as hotcue X of type “Hotcue”. | binary | missing | (Hotcue sets an empty pad); *a small engine or app feature* | 15 (0 / 0 / 15) |
| `cue_preview` | Plays from the current cue point. | binary | missing | *a small engine or app feature* | 14 (3 / 0 / 11) |
| `beatlooproll_activate` | Activates a rolling loop over beatloop_size beats. | binary | missing | *slip, reverse and rolls* | 13 (1 / 0 / 12) |
| `reset_key` | Resets the key to the original track key. | binary | missing | *key shift and key sync* | 13 (1 / 1 / 12) |
| `scratch2_enable` | Takes over play speed & direction for [ChannelN],scratch2. | binary | missing | *jog wheels and scratching* | 13 (2 / 0 / 11) |
| `loop_move` | Move loop forward by X beats (positive) or backward by X beats (negative). | real number | missing | *a small engine or app feature* | 12 (0 / 0 / 12) |
| `sync_key` | Match musical key. | value | missing | *key shift and key sync* | 12 (1 / 1 / 11) |
| `scratch2` | Affects absolute play speed & direction whether currently playing or not when… | -3.0..3.0 | missing | *jog wheels and scratching* | 11 (0 / 0 / 11) |
| `beats_translate_match_alignment` | Adjust beatgrid to match another playing deck. | binary | missing | *a small engine or app feature* | 9 (1 / 0 / 8) |
| `mute` † | Mutes the channel | binary | missing | *a small engine or app feature* | 9 (1 / 0 / 8) |
| `passthrough` | Connects the vinyl control input for vinyl control on that deck to the channel output. | binary | missing | *timecode vinyl* | 9 (1 / 1 / 9) |
| `wheel` | Affects relative playback speed and direction persistently (additive offset & must… | -3.0..3.0 | missing | *jog wheels and scratching* | 9 (1 / 0 / 8) |
| `key` | Current key of the track | enum | missing | (key is analysed; no key shift); *key shift and key sync* | 8 (0 / 0 / 8) |
| `pitch_down` | Changes the track pitch down one half step, independent of the tempo. | binary | missing | *key shift and key sync* | 8 (1 / 1 / 7) |
| `pitch_up` | Changes the track pitch up one half step, independent of the tempo. | binary | missing | *key shift and key sync* | 8 (1 / 1 / 7) |
| `vinylcontrol_enabled` | Toggles whether a deck is being controlled by digital vinyl. | binary | missing | *timecode vinyl* | 7 (1 / 1 / 6) |
| `vinylcontrol_mode` | Determines how vinyl control interprets needle information. | enum | missing | *timecode vinyl* | 6 (1 / 1 / 5) |
| `beatsync_phase` | Syncs the phase to that of the other track (if BPM is detected on both). | binary | missing | *a small engine or app feature* | 5 (0 / 0 / 5) |
| `beatsync_tempo` | Syncs the tempo to that of the other track (if BPM is detected on both). | binary | missing | *a small engine or app feature* | 5 (0 / 1 / 5) |
| `cue_play` | Go to cue point and play after release (CUP button behavior). | binary | missing | *a small engine or app feature* | 5 (2 / 0 / 4) |
| `loop_move_N_backward` | Loop moves by X beats. | binary | missing | *a small engine or app feature* | 5 (0 / 0 / 5) |
| `loop_move_N_forward` | Moves the loop in and out points forward by X beats. | binary | missing | *a small engine or app feature* | 5 (0 / 0 / 5) |
| `beat_closest` | Its value is set to the sample position of the closest beat of the active beat and is… | -1, 0.0, real-valued | missing | (grid in DeckSession); *an LED output worked out in the app* | 4 (0 / 0 / 4) |
| `cue_simple` | If the player is not playing, set the cue point at the current location otherwise seek… | binary | missing | *a small engine or app feature* | 4 (1 / 0 / 3) |
| `vinylcontrol_cueing` | Determines how cue points are treated in vinyl control relative mode. | enum | missing | *timecode vinyl* | 4 (0 / 0 / 4) |
| `vinylcontrol_status` | Provides visual feedback with regards to vinyl control status. | read-only 0.0-3.0 | missing | *timecode vinyl* | 4 (0 / 0 / 4) |
| `CloneFromDeck` | Clone the given deck number, copying the play state, position, rate, and key. | integer | missing | *a small engine or app feature* | 3 (0 / 0 / 3) |
| `beat_distance` | Outputs the relative position of the play marker in the section between the previous and… | 0.0 - 1.0, real-valued | missing | (grid in DeckSession); *an LED output worked out in the app* | 3 (0 / 0 / 3) |
| `loop_in_goto` | Seek to the loop in point. | binary | missing | *a small engine or app feature* | 3 (1 / 0 / 2) |
| `visual_key_distance` ± | The distance to the nearest key measured in cents. | -0.5..0.5 | missing | *key shift and key sync* | 3 (0 / 0 / 3) |
| `hotcue_focus_color_next` | If there is a focused hotcue, sets its color to the next color in the palette. | binary | missing | *a small engine or app feature* | 2 (0 / 0 / 2) |
| `hotcue_focus_color_prev` | If there is a focused hotcue, sets its color to the previous color in the palette. | binary | missing | *a small engine or app feature* | 2 (0 / 0 / 2) |
| `intro_end_activate` | If the intro end cue is set, seeks the player to the intro end position. | binary | missing | *intro / outro markers* | 2 (0 / 0 / 2) |
| `intro_start_activate` | If the intro start cue is set, seeks the player to the intro start position. | binary | missing | *intro / outro markers* | 2 (0 / 0 / 2) |
| `loop_out_goto` | Seek to the loop out point. | binary | missing | *a small engine or app feature* | 2 (1 / 0 / 1) |
| `outro_end_activate` | If the outro end cue is set, seeks the player to the outro end position. | binary | missing | *intro / outro markers* | 2 (0 / 0 / 2) |
| `outro_start_activate` | If the outro start cue is set, seeks the player to the outro start position. | binary | missing | *intro / outro markers* | 2 (0 / 0 / 2) |
| `hotcue_N_activateloop` | Identical to hotcue_X_activate, but this always sets a saved loop, regardless of whether… | value | missing | *a small engine or app feature* | 1 (0 / 0 / 1) |
| `hotcue_N_cueloop` | Enables or disables a loop from the position of hotcue X. | value | missing | *a small engine or app feature* | 1 (0 / 0 / 1) |
| `hotcue_N_gotoandloop` | If hotcue X is set, seeks the player to hotcue X’s position, starts playback and looping. | binary | missing | *a small engine or app feature* | 1 (0 / 0 / 1) |
| `hotcue_focus` | Contains the number of the most recently used hotcue (or -1 if no hotcue was used). | integer | missing | *a small engine or app feature* | 1 (0 / 0 / 1) |
| `intro_end_enabled` | 1 if intro end cue is set, (position is not -1), 0 otherwise. | read-only binary | missing | *intro / outro markers* | 1 (0 / 0 / 1) |
| `intro_start_enabled` | 1 if intro start cue is set, (position is not -1), 0 otherwise. | read-only binary | missing | *intro / outro markers* | 1 (0 / 0 / 1) |
| `loop_remove` | Clears the last active loop, i.e. | binary | missing | *a small engine or app feature* | 1 (0 / 0 / 1) |
| `outro_end_enabled` | 1 if outro end cue is set, (position is not -1), 0 otherwise. | read-only binary | missing | *intro / outro markers* | 1 (0 / 0 / 1) |
| `outro_start_enabled` | 1 if outro start cue is set, (position is not -1), 0 otherwise. | read-only binary | missing | *intro / outro markers* | 1 (0 / 0 / 1) |
| `rateSearch` ± | Seeks forward (positive values) or backward (negative values) at a speed determined by… | -300..300 | missing | (Seek exists); *a small engine or app feature* | 1 (0 / 0 / 1) |
| `shift_cues_earlier` | (No description) | binary | missing | *a small engine or app feature* | 1 (0 / 0 / 1) |
| `shift_cues_later` | (No description) | binary | missing | *a small engine or app feature* | 1 (0 / 0 / 1) |
| `show_track_menu` † | Toggle the track context menu for the track currently loaded in this deck. | binary | missing | *library or window navigation* | 1 (0 / 0 / 1) |
| `update_replaygain_from_pregain` | Applies the deck pregain knob value to the detected ReplayGain value for the current… | binary | missing | *a small engine or app feature* | 1 (0 / 0 / 1) |
| `CloneFromSampler` | Clone the given sampler number, copying the play state, position, rate, and key. | integer | missing | *sampler decks* | – |
| `LoadTrackFromDeck` | Load the track currently loaded to the given deck number. | integer | missing | *a small engine or app feature* | – |
| `LoadTrackFromPreviewDeck` | Load the track currently loaded to the given preview deck number. | integer | missing | *a small engine or app feature* | – |
| `LoadTrackFromSampler` | Load the track currently loaded to the given sampler number. | integer | missing | *sampler decks* | – |
| `beatloop_rN_activate` | Activates a loop over X beats backwards from the current position. | binary | missing | *a small engine or app feature* | – |
| `beatlooproll_rN_activate` | Activates a rolling loop over X beats backwards from the current position. | binary | missing | *slip, reverse and rolls* | – |
| `beats_translate_half` | Move beatgrid by exactly half a beat. | binary | missing | *a small engine or app feature* | – |
| `bpmlock` | Toggle the beatgrid/BPM lock state. | binary | missing | *a small engine or app feature* | – |
| `cue_clear` | Deletes the already set cue point and sets [ChannelN],cue_point to -1. | binary | missing | *a small engine or app feature* | – |
| `hotcue_N_setcue` | Identical to hotcue_X_set, but this always sets a regular cue point (i.e. | value | missing | (Hotcue sets an empty pad); *a small engine or app feature* | – |
| `hotcue_N_setloop` | Identical to hotcue_X_set, but this always saves a loop (i.e. | value | missing | *a small engine or app feature* | – |
| `hotcue_N_swap` | Swap numbers of two hotcues. | integer | missing | *a small engine or app feature* | – |
| `intro_end_clear` | If the intro end cue is set, clears its status. | binary | missing | *intro / outro markers* | – |
| `intro_end_position` | The position of the intro end in samples, -1 if not set. | integer | missing | *intro / outro markers* | – |
| `intro_end_set` | Set intro end to the current play position. | binary | missing | *intro / outro markers* | – |
| `intro_start_clear` | If the intro start cue is set, clears its status. | binary | missing | *intro / outro markers* | – |
| `intro_start_position` | The position of the intro start in samples, -1 if not set. | integer | missing | *intro / outro markers* | – |
| `intro_start_set` | Set intro start to the current play position. | binary | missing | *intro / outro markers* | – |
| `loop_anchor` | Adjusts whether loops created with [ChannelN],beatloop_X_activate,… | enum | missing | *a small engine or app feature* | – |
| `outro_end_clear` | If the outro end cue is set, clears its status. | binary | missing | *intro / outro markers* | – |
| `outro_end_position` | The position of the outro end in samples, -1 if not set. | integer | missing | *intro / outro markers* | – |
| `outro_end_set` | Set outro end to the current play position. | binary | missing | *intro / outro markers* | – |
| `outro_start_clear` | If the outro start cue is set, clears its status. | binary | missing | *intro / outro markers* | – |
| `outro_start_position` | The position of the outro start in samples, -1 if not set. | integer | missing | *intro / outro markers* | – |
| `outro_start_set` | Set outro start to the current play position. | binary | missing | *intro / outro markers* | – |
| `scratch_position` | Affects absolute play speed & direction whether currently playing or not when… | double | missing | *jog wheels and scratching* | – |
| `scratch_position_enable` | When active this enables scratching with [ChannelN],scratch_position. | binary | missing | *jog wheels and scratching* | – |
| `shift_cues_earlier_small` | (No description) | binary | missing | *a small engine or app feature* | – |
| `shift_cues_later_small` | (No description) | binary | missing | *a small engine or app feature* | – |
| `sort_hotcues` | Sort hotcues by their position in the track. | binary | missing | *a small engine or app feature* | – |
| `sort_hotcues_remove_offsets` | Sort hotcues by their position in the track. | binary | missing | *a small engine or app feature* | – |
| `stem_count` | The number of stem track available for the loaded file. | read-only 0, 2..4 | missing | *stems* | – |
| `tempo_tap` | When tapped repeatedly, adjusts the rate/tempo of the deck to match the taps. | binary | missing | *a small engine or app feature* | – |
| `cue_mode` | Represents the currently chosen cue mode. | enum | n/a | earshot has one cue mode (CDJ) | – |
| `VuMeterN` † | Outputs the current instantaneous deck volume for the left channel | value | partial | DeckSnap.vu (mono); *a small engine or app feature*; deprecated | 10 (0 / 0 / 10) |
| `PeakIndicatorN` † | Indicates when the signal is clipping (too loud for the hardware and is being distorted)… | binary | partial | DeckSnap.vu (mono); *a small engine or app feature*; deprecated | 2 (0 / 0 / 2) |
| `reloop_exit` | Toggles the current loop on or off. | binary | name | Reloop; alias of reloop_toggle | 77 (26 / 1 / 53) |
| `hotcue_N_enabled` | Indicates if hotcue slot X is set, active or empty. | value | name | DeckControl::Hotcue get; alias of hotcue_N_status | 69 (0 / 23 / 51) |
| `filterMid` | Adjusts the gain of the mid EQ filter. | 0.0..1.0..4.0 | name | Eq(EQ_MID); alias of the EQ rack's parameter2 | 59 (42 / 0 / 22) |
| `filterHigh` | Adjusts the gain of the high EQ filter. | 0.0..1.0..4.0 | name | Eq(EQ_HIGH); alias of the EQ rack's parameter3 | 58 (42 / 0 / 22) |
| `filterLow` | Adjusts the gain of the low EQ filter. | 0.0..1.0..4.0 | name | Eq(EQ_LOW); alias of the EQ rack's parameter1 | 58 (42 / 0 / 22) |
| `beatloop_N` | Setup a loop over X beats. | binary | name | BeatLoop(n); deprecated | 37 (5 / 2 / 32) |
| `VuMeter` † | Outputs the current instantaneous deck volume | value | name | DeckSnap.vu; deprecated | 10 (0 / 0 / 10) |
| `beatloop` | Setup a loop over the set number of beats. | positive real number | name | BeatLoop; deprecated | 10 (0 / 0 / 10) |
| `PeakIndicator` † | Indicates when the signal is clipping (too loud for the hardware and is being distorted) | binary | name | DeckSnap.vu; deprecated | 2 (0 / 0 / 2) |
| `filterDepth` | Adjusts the intensity of the filter effect. | value | name | Filter; deprecated | – |
| `filterHighKill` | Holds the gain of the high EQ to -inf while active. | binary | missing | *a small engine or app feature*; deprecated | 38 (15 / 11 / 25) |
| `filterLowKill` | Holds the gain of the low EQ to -inf while active | binary | missing | *a small engine or app feature*; deprecated | 38 (17 / 11 / 23) |
| `filterMidKill` | Holds the gain of the mid EQ to -inf while active. | binary | missing | *a small engine or app feature*; deprecated | 38 (15 / 11 / 25) |
| `filter` | Toggles the filter effect. | binary | missing | (filter has no on/off); *a small engine or app feature*; deprecated | 3 (1 / 0 / 2) |

### Mixer: [Master], [Main]

Groups: `[Main]`, `[Master]`.

| Control | What it does | Type | earshot | Equivalent / needs | Used by (XML in / out / JS) |
|---|---|---|---|---|---|
| `crossfader` ± | Adjusts the crossfader between players/decks (-1.0 is all the way left). | -1.0..1.0 | **have** | Master(Crossfader) | 125 (87 / 0 / 47) |
| `headMix` ± | Adjusts the cue/main mix in the headphone output. | value | **have** | Master(HeadMix) | 88 (52 / 0 / 43) |
| `headGain` ± | Adjusts the headphone output gain. | 0.0..1.0..5.0 | **have** | Master(HeadGain) | 27 (14 / 0 / 14) |
| `balance` ± | Adjusts the left/right channel balance on the Main output. | -1.0..1.0 | **have** | Master(Balance) | 24 (10 / 0 / 14) |
| `gain` ± † | Adjusts the gain for the main output as well as recording and broadcasting signal. | 0.0..1.0..5.0 | **have** | Master(Gain) | 24 (11 / 0 / 14) |
| `enabled` † | Indicator that the main mix is processed. | binary | name | constant 1 | 48 (0 / 0 / 48) |
| `vu_meter` † | Outputs the current instantaneous main volume (composite). | value | name | MixerSnap.vu_main | 38 (0 / 0 / 38) |
| `peak_indicator` † | Indicates when the signal is clipping (too loud for the hardware and is being distorted)… | binary | name | MixerSnap.vu_main / limiting | 24 (0 / 0 / 24) |
| `vu_meter_left` † | Outputs the current instantaneous main volume for the left channel. | value | name | MixerSnap.vu_main[0] | 20 (0 / 0 / 20) |
| `vu_meter_right` † | Outputs the current instantaneous main volume for the right channel. | value | name | MixerSnap.vu_main[1] | 20 (0 / 0 / 20) |
| `peak_indicator_left` † | Indicates when the signal is clipping (too loud for the hardware and is being distorted)… | binary | name | MixerSnap.vu_main[0] | 4 (0 / 0 / 4) |
| `peak_indicator_right` † | Indicates when the signal is clipping (too loud for the hardware and is being distorted)… | binary | name | MixerSnap.vu_main[1] | 4 (0 / 0 / 4) |
| `crossfader_down_small` | Moves the crossfader left by 1/100th. | binary | name | Master(Crossfader) | 1 (0 / 0 / 1) |
| `crossfader_up_small` | Moves the crossfader right by 1/100th. | binary | name | Master(Crossfader) | 1 (0 / 0 / 1) |
| `crossfader_down` | Moves the crossfader left by 1/10th. | binary | name | Master(Crossfader) | – |
| `crossfader_up` | Moves the crossfader right by 1/10th. | binary | name | Master(Crossfader) | – |
| `guiTick50ms` | A throttled timer that provides the time elapsed in seconds since Mixxx was started. | read-only 0.0 .. n | name | a UI clock; deprecated | – |
| `guiTickTime` | A high-resolution timer that provides the elapsed time in seconds since Mixxx was started. | read-only 0.0 .. n | name | a UI clock; deprecated | – |
| `headEnabled` | Indicator that the headphone mix is processed. | binary | name | routing has a headphone bus | – |
| `latency` | Latency setting (sound buffer size) in milliseconds (default 64). | >=0 (absolute value) | name | buffer size / sample rate | – |
| `num_effectsavailable` | The number of available effects that can be selected in an effect slot. | read-only integer | missing | *effect units* | 12 (0 / 0 / 12) |
| `headSplit` | Splits headphone stereo cueing into right (main mono) and left (PFL mono). | binary | missing | *a small engine or app feature* | 8 (1 / 0 / 7) |
| `booth_gain` ± | Adjusts the gain of the Booth output. | 0.0…1.0…5.0 | missing | *a small engine or app feature* | 2 (0 / 0 / 2) |
| `booth_enabled` | Indicates whether a Booth output is configured in the Sound Hardware Preferences. | binary | missing | *a small engine or app feature* | – |
| `duckStrength` ± | Microphone ducking strength. | 0.0..1.0 | missing | *microphone and aux inputs* | – |
| `talkoverDucking` | Toggle microphone ducking mode (OFF, AUTO, MANUAL) | value | missing | *microphone and aux inputs* | – |
| `volume` † | Adjust main volume. | 0.0..1.0..5.0 | name | Master(Gain); deprecated alias | 97 (31 / 0 / 72) |
| `headVolume` | Adjust headphone volume. | 0.0..1.0..5.0 | name | Master(HeadGain); deprecated alias | 39 (26 / 0 / 14) |
| `num_decks` † | The number of decks currently enabled. | integer | name | EngineSnapshot.decks_live; deprecated | 13 (0 / 0 / 13) |
| `VuMeter` † | Outputs the current instantaneous main volume (composite). | value | name | MixerSnap.vu_main; deprecated | 10 (0 / 0 / 10) |
| `VuMeterN` † | Outputs the current instantaneous main volume for the left channel. | value | name | MixerSnap.vu_main; deprecated | 10 (0 / 0 / 10) |
| `PeakIndicator` † | Indicates when the signal is clipping (too loud for the hardware and is being distorted)… | binary | name | MixerSnap.vu_main; deprecated | 2 (0 / 0 / 2) |
| `PeakIndicatorN` † | Indicates when the signal is clipping (too loud for the hardware and is being distorted)… | binary | name | MixerSnap.vu_main; deprecated | 2 (0 / 0 / 2) |
| `audio_latency_overload` † | Indicates a buffer under or over-flow. | binary | name | Stats.xruns; deprecated | 1 (0 / 0 / 1) |
| `samplerate` † | The current output sample rate (default: 44100 Hz). | absolute value (in Hz) | name | EngineSnapshot.sample_rate; deprecated | 1 (0 / 0 / 1) |
| `audio_latency_overload_count` † | Counts buffer over- and underflows, maximum one per 500 ms. | 0 .. n | name | Stats.xruns; deprecated | – |
| `audio_latency_usage` † | Reflects fraction of latency, given by the audio buffer size, spend for audio processing… | 0 .. 25 % | name | Stats.max_callback_ns; deprecated | – |
| `num_auxiliaries` † | The number of auxiliary inputs that can be configured. | integer | name | 0; deprecated | – |
| `num_microphones` † | The number of microphone inputs that can be configured. | integer | name | 0; deprecated | – |
| `num_preview_decks` † | The number of preview decks currently enabled. | integer | name | 1; deprecated | – |
| `num_samplers` † | The number of samplers currently enabled. | integer | missing | *sampler decks*; deprecated | 46 (0 / 0 / 46) |
| `maximize_library` | Toggle maximized view of library. | binary | n/a | deprecated skin control | 3 (0 / 0 / 3) |

### [App]

Groups: `[App]`.

| Control | What it does | Type | earshot | Equivalent / needs | Used by (XML in / out / JS) |
|---|---|---|---|---|---|
| `num_decks` † | The number of decks currently enabled. | integer | name | EngineSnapshot.decks_live | 13 (0 / 0 / 13) |
| `gui_tick_50ms_period_s` | A throttled timer that provides the time elapsed in seconds since Mixxx was started. | read-only 0.0 .. n | name | a UI clock | 3 (0 / 0 / 3) |
| `indicator_250ms` | Alternates between 0.0 and 1.0 every 250 milliseconds. | read-only binary | name | a UI clock | 2 (0 / 0 / 2) |
| `audio_latency_overload` † | Indicates a buffer under or over-flow. | binary | name | Stats.xruns | 1 (0 / 0 / 1) |
| `indicator_500ms` | Alternates between 0.0 and 1.0 every 500 milliseconds. | read-only binary | name | a UI clock | 1 (0 / 0 / 1) |
| `samplerate` † | The current output sample rate (default: 44100 Hz). | absolute value (in Hz) | name | EngineSnapshot.sample_rate | 1 (0 / 0 / 1) |
| `audio_latency_overload_count` † | Counts buffer over and under-flows, maximum one per 500 ms. | 0 .. n | name | Stats.xruns | – |
| `audio_latency_usage` † | Reflects fraction of latency, given by the audio buffer size, spend for audio processing… | 0 .. 25 % | name | Stats.max_callback_ns | – |
| `gui_tick_full_period_s` | A high-resolution timer that provides the elapsed time in seconds since Mixxx was started. | read-only 0.0 .. n | name | a UI clock | – |
| `num_auxiliaries` † | The number of auxiliary inputs that can be configured. | integer | name | 0 | – |
| `num_microphones` † | The number of microphone inputs that can be configured. | integer | name | 0 | – |
| `num_preview_decks` † | The number of preview decks currently enabled. | integer | name | 1 (Prepare's player) | – |
| `num_samplers` † | The number of samplers currently enabled. | integer | missing | *sampler decks* | 46 (0 / 0 / 46) |

### Library: [Library], [Playlist]

Groups: `[Library]`, `[Playlist]`.

| Control | What it does | Type | earshot | Equivalent / needs | Used by (XML in / out / JS) |
|---|---|---|---|---|---|
| `MoveVertical` | Move the specified number of locations up or down. | relative | **have** | Browse | 28 (9 / 0 / 20) |
| `GoToItem` | Triggers different actions, depending on which interface element currently has keyboard… | binary | partial | LoadSelected / open the source; *library or window navigation* | 23 (5 / 0 / 18) |
| `MoveFocusForward` | Equivalent to pressing the Tab key on the keyboard | binary | partial | TogglePane / FocusPane | 15 (4 / 0 / 11) |
| `MoveFocus` | Move focus the specified number of panes forward or backwards. | relative | partial | TogglePane / FocusPane; earshot's panes differ from Mixxx's sidebar + table | 14 (5 / 3 / 9) |
| `MoveFocusBackward` | Equivalent to pressing the Shift + Tab key on the keyboard | binary | partial | TogglePane / FocusPane | 13 (4 / 0 / 9) |
| `SelectNextTrack` | Scrolls to the next track in the track table. | value | name | Browse(1) | 57 (21 / 0 / 40) |
| `SelectPrevTrack` | Scrolls to the previous track in the track table. | value | name | Browse(-1) | 57 (21 / 0 / 40) |
| `SelectTrackKnob` | Scrolls the given number of tracks in the track table (can be negative for reverse… | relative | name | Browse | 50 (9 / 0 / 41) |
| `LoadSelectedIntoFirstStopped` | Performs the same action action like [Library],GoToItem does when the tracks table has… | value | name | LoadSelected (first stopped deck) | 24 (12 / 0 / 13) |
| `MoveDown` | Equivalent to pressing the Down key on the keyboard | binary | name | Browse(1) | 9 (0 / 0 / 9) |
| `MoveUp` | Equivalent to pressing the Up key on the keyboard | binary | name | Browse(-1) | 9 (0 / 0 / 9) |
| `ScrollVertical` | Scroll the specified number of pages up or down. | relative | name | Browse (by a page) | 8 (4 / 0 / 4) |
| `ScrollDown` | Equivalent to pressing the PageDown key on the keyboard | binary | name | Browse (by a page) | 2 (0 / 0 / 2) |
| `ScrollUp` | Equivalent to pressing the PageUp key on the keyboard | binary | name | Browse (by a page) | 2 (0 / 0 / 2) |
| `track_color_next` | Set color of selected track to next color in palette. | binary | name | Edit Colour | 2 (0 / 0 / 2) |
| `track_color_prev` | Set color of selected track to previous color in palette. | binary | name | Edit Colour | 2 (0 / 0 / 2) |
| `stars_down` † | Decrease the rating of the currently selected track(s). | binary | name | Edit Rating | 1 (0 / 0 / 1) |
| `stars_up` † | Increase the rating of the currently selected track(s). | binary | name | Edit Rating | 1 (0 / 0 / 1) |
| `SelectNextPlaylist` | Switches to the next view (Library, Queue, etc.). | value | missing | (Source exists); *library or window navigation* | 53 (12 / 0 / 43) |
| `SelectPrevPlaylist` | Switches to the previous view (Library, Queue, etc.). | binary | missing | (Source exists); *library or window navigation* | 51 (11 / 0 / 42) |
| `ToggleSelectedSidebarItem` | Toggles (expands/collapses) the currently selected sidebar item. | value | missing | *library or window navigation* | 20 (3 / 0 / 17) |
| `MoveHorizontal` | Move the specified number of locations left or right. | relative | missing | *library or window navigation* | 6 (2 / 0 / 4) |
| `MoveLeft` | Equivalent to pressing the Left key on the keyboard | binary | missing | *library or window navigation* | 6 (2 / 0 / 4) |
| `MoveRight` | Equivalent to pressing the Right key on the keyboard | binary | missing | *library or window navigation* | 6 (2 / 0 / 4) |
| `AutoDjAddBottom` | Add selected track(s) to Auto DJ Queue (bottom). | binary | missing | *Auto DJ*; in [Library], [Playlist] | 5 (0 / 0 / 5) |
| `AutoDjAddTop` | Add selected track(s) to Auto DJ Queue (top). | binary | missing | *Auto DJ*; in [Library], [Playlist] | 5 (0 / 0 / 5) |
| `SelectPlaylist` | Scrolls the given number of items (view, playlist, crate, etc.) in the side pane (can be… | relative | missing | (Source exists); *library or window navigation* | 4 (0 / 0 / 4) |
| `focused_widget` | Read this control to know which library widget is currently focused, or write in order… | enum | missing | *library or window navigation* | 4 (0 / 0 / 4) |
| `sort_column_toggle` | Equivalent to clicking on column headers. | value | missing | *library or window navigation* | 4 (0 / 0 / 4) |
| `clear_search` | Clear the search. | binary | missing | *library or window navigation* | 2 (0 / 0 / 2) |
| `search_history_selector` | Select another saved search query. | relative | missing | *library or window navigation* | 2 (0 / 0 / 2) |
| `sort_order` | Indicate the sort order of the track tables. | binary | missing | *library or window navigation* | 2 (0 / 0 / 2) |
| `show_track_menu` † | Toggle the track context menu for all tracks selected in the current library view. | binary | missing | *library or window navigation* | 1 (0 / 0 / 1) |
| `sort_column` | Indicates the sorting column the track table | enum | missing | *library or window navigation* | 1 (0 / 0 / 1) |
| `search_history_next` | Select the next saved search query. | binary | missing | *library or window navigation* | – |
| `search_history_prev` | Select the previous saved search query. | binary | missing | *library or window navigation* | – |
| `set_bpmlock` | Set the beatgrid/BPM lock state of the selected track(s). | binary | missing | *a small engine or app feature* | – |
| `sort_focused_column` | Sort the column of the table cell that is currently focused, which is equivalent to… | binary | missing | *library or window navigation* | – |
| `font_size_knob` | Increase or decrease the size of the library font | relative | n/a |  | 1 (0 / 0 / 1) |
| `font_size_decrement` | Decrease the size of the library font | binary | n/a |  | – |
| `font_size_increment` | Increase the size of the library font. | binary | n/a |  | – |
| `show_coverart` | Toggle the Cover Art in Library | binary | n/a | deprecated skin control | – |

### Effects: [EffectRack1_…], [EqualizerRack1_…], [QuickEffectRack1_…]

Groups: `[EffectRack1]`, `[EffectRack1_EffectUnitN]`, `[EffectRack1_EffectUnitN_EffectM]`, `[EqualizerRack1]`, `[EqualizerRack1_[ChannelI]]`, `[EqualizerRack1_[ChannelI]_Effect1]`, `[QuickEffectRack1]`, `[QuickEffectRack1_[ChannelI]]`, `[QuickEffectRack1_[ChannelI]_Effect1]`, `[QuickEffectRack1_[ChannelI_StemJ]]`, `[QuickEffectRack1_[ChannelI_StemJ]_Effect1]`.

| Control | What it does | Type | earshot | Equivalent / needs | Used by (XML in / out / JS) |
|---|---|---|---|---|---|
| `parameterN` ± | The scaled value of the Kth parameter. | double | partial | Eq(band); have for the EQ rack's parameter1-3; missing for effect units; in [EffectRack1_EffectUnitN_EffectM], [EqualizerRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI_StemJ]_Effect1] | 72 (27 / 0 / 52) |
| `super1` ± | The EffectChain superknob. | 0.0..1.0 | partial | Filter; have for [QuickEffectRack1_[ChannelN]] (the filter); missing for effect units; in [EffectRack1_EffectUnitN], [EqualizerRack1_[ChannelI]], [QuickEffectRack1_[ChannelI]], [QuickEffectRack1_[ChannelI_StemJ]] | 72 (17 / 0 / 60) |
| `enabled` † | If true, the effect in this slot will be processed. | binary | missing | *effect units*; in [EffectRack1_EffectUnitN_EffectM], [EqualizerRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI_StemJ]_Effect1] | 57 (11 / 13 / 48) |
| `mix` ± | The dry/wet mixing ratio for this EffectChain with the EngineChannels it is mixed with. | 0.0..1.0 | missing | *effect units*; in [EffectRack1_EffectUnitN], [EqualizerRack1_[ChannelI]], [QuickEffectRack1_[ChannelI]], [QuickEffectRack1_[ChannelI_StemJ]] | 55 (5 / 0 / 50) |
| `enabled` † | If true, the EffectChain in this EffectUnit will be processed. | binary | missing | *effect units*; in [EffectRack1_EffectUnitN], [EqualizerRack1_[ChannelI]], [QuickEffectRack1_[ChannelI]], [QuickEffectRack1_[ChannelI_StemJ]] | 51 (4 / 3 / 48) |
| `group_[ChannelN]_enable` | Whether or not this EffectChain applies to Deck I | binary | missing | *effect units*; in [EffectRack1_EffectUnitN], [EqualizerRack1_[ChannelI]], [QuickEffectRack1_[ChannelI]], [QuickEffectRack1_[ChannelI]], [QuickEffectRack1_[ChannelI_StemJ]] | 40 (13 / 13 / 28) |
| `meta` ± | Controls the parameters that are linked to the metaknob. | 0..1 | missing | *effect units*; in [EffectRack1_EffectUnitN_EffectM], [EqualizerRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI_StemJ]_Effect1] | 40 (4 / 0 / 37) |
| `focused_effect` | 0 indicates no effect is focused; > 0 indicates the index of the focused effect. | 0..num_effectslots | missing | *effect units*; in [EffectRack1_EffectUnitN], [EqualizerRack1_[ChannelI]], [QuickEffectRack1_[ChannelI]], [QuickEffectRack1_[ChannelI_StemJ]] | 27 (0 / 0 / 27) |
| `button_parameterN` | The value of the Kth parameter. | double | missing | (EQ kills); *effect units*; in [EffectRack1_EffectUnitN_EffectM], [EqualizerRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI_StemJ]_Effect1] | 25 (5 / 4 / 23) |
| `effect_selector` | Select Effect – >0 goes one forward, <0 goes one backward. | relative | missing | *effect units*; in [EffectRack1_EffectUnitN_EffectM], [EqualizerRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI_StemJ]_Effect1] | 24 (3 / 0 / 21) |
| `show_focus` | Whether to show focus buttons and draw a border around the focused effect in skins. | binary | missing | *effect units*; in [EffectRack1_EffectUnitN], [EqualizerRack1_[ChannelI]], [QuickEffectRack1_[ChannelI]], [QuickEffectRack1_[ChannelI_StemJ]] | 23 (0 / 0 / 23) |
| `show_parameters` | Whether to show all the parameters of each effect in skins or only show metaknobs. | binary | missing | *effect units*; in [EffectRack1_EffectUnitN], [EqualizerRack1_[ChannelI]], [QuickEffectRack1_[ChannelI]], [QuickEffectRack1_[ChannelI_StemJ]] | 18 (1 / 0 / 17) |
| `next_effect` | Cycle to the next effect after the currently loaded effect. | binary | missing | *effect units*; in [EffectRack1_EffectUnitN_EffectM], [EqualizerRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI_StemJ]_Effect1] | 14 (4 / 1 / 10) |
| `parameterN_loaded` | Whether or not the Kth parameter slot has an effect parameter loaded into it. | read-only binary | missing | *effect units*; in [EffectRack1_EffectUnitN_EffectM], [EqualizerRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI_StemJ]_Effect1] | 14 (0 / 0 / 14) |
| `button_parameterN_loaded` | Whether or not the Kth parameter slot has an effect parameter loaded into it. | read-only binary | missing | *effect units*; in [EffectRack1_EffectUnitN_EffectM], [EqualizerRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI_StemJ]_Effect1] | 7 (0 / 0 / 7) |
| `clear` | Clear the currently loaded EffectChain in this EffectUnit. | binary | missing | *effect units*; in [EffectRack1_EffectUnitN], [EqualizerRack1_[ChannelI]], [QuickEffectRack1_[ChannelI]], [QuickEffectRack1_[ChannelI_StemJ]] | 7 (0 / 0 / 7) |
| `clear` | Clear the currently loaded Effect in this Effect slot from the EffectUnit. | binary | missing | *effect units*; in [EffectRack1_EffectUnitN_EffectM], [EqualizerRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI_StemJ]_Effect1] | 7 (0 / 0 / 7) |
| `num_parameters` | The number of parameters the currently loaded effect has. | read-only integer | missing | *effect units*; in [EffectRack1_EffectUnitN_EffectM], [EqualizerRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI_StemJ]_Effect1] | 7 (0 / 0 / 7) |
| `prev_effect` | Cycle to the previous effect before the currently loaded effect. | binary | missing | *effect units*; in [EffectRack1_EffectUnitN_EffectM], [EqualizerRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI_StemJ]_Effect1] | 7 (0 / 0 / 7) |
| `group_[Headphone]_enable` | Whether or not this EffectChain applies to the Headphone output | binary | missing | *effect units* | 6 (1 / 1 / 5) |
| `group_[Master]_enable` | Whether or not this EffectChain applies to the Main output | binary | missing | *effect units* | 6 (2 / 2 / 4) |
| `loaded` | Whether an EffectChain is loaded into the EffectUnit | read-only binary | missing | *effect units*; in [EffectRack1_EffectUnitN], [EqualizerRack1_[ChannelI]], [QuickEffectRack1_[ChannelI]], [QuickEffectRack1_[ChannelI_StemJ]] | 6 (0 / 0 / 6) |
| `loaded` | Whether an Effect is loaded into this EffectSlot | read-only binary | missing | *effect units*; in [EffectRack1_EffectUnitN_EffectM], [EqualizerRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI_StemJ]_Effect1] | 6 (0 / 0 / 6) |
| `parameterN_link_type` | The link type of the Kth parameter to the effects’s metaknob. | enum | missing | *effect units*; in [EffectRack1_EffectUnitN_EffectM], [EqualizerRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI_StemJ]_Effect1] | 6 (0 / 0 / 6) |
| `loaded_chain_preset` | 0-based index of the currently loaded EffectChain preset. | integer | missing | *effect units*; in [EffectRack1_EffectUnitN], [EqualizerRack1_[ChannelI]], [QuickEffectRack1_[ChannelI]], [QuickEffectRack1_[ChannelI_StemJ]] | 5 (0 / 0 / 5) |
| `num_button_parameters` | The number of button parameters the currently loaded effect has. | read-only integer | missing | *effect units*; in [EffectRack1_EffectUnitN_EffectM], [EqualizerRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI_StemJ]_Effect1] | 5 (0 / 0 / 5) |
| `parameterN_type` | The type of the Kth parameter value. | read-only integer | missing | *effect units*; in [EffectRack1_EffectUnitN_EffectM], [EqualizerRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI_StemJ]_Effect1] | 5 (0 / 0 / 5) |
| `num_effectslots` | The number of effect slots available in this EffectUnit. | read-only integer | missing | *effect units*; in [EffectRack1_EffectUnitN], [EqualizerRack1_[ChannelI]], [QuickEffectRack1_[ChannelI]], [QuickEffectRack1_[ChannelI_StemJ]] | 4 (0 / 0 / 4) |
| `parameterN_link_inverse` | The link direction of the Kth parameter to the effect’s metaknob. | binary | missing | *effect units*; in [EffectRack1_EffectUnitN_EffectM], [EqualizerRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI_StemJ]_Effect1] | 4 (0 / 0 / 4) |
| `button_parameterN_type` | The type of the Kth parameter value. | read-only integer | missing | *effect units*; in [EffectRack1_EffectUnitN_EffectM], [EqualizerRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI_StemJ]_Effect1] | 3 (0 / 0 / 3) |
| `chain_preset_selector` | Select EffectChain preset. | relative | missing | *effect units*; in [EffectRack1_EffectUnitN], [EqualizerRack1_[ChannelI]], [QuickEffectRack1_[ChannelI]], [QuickEffectRack1_[ChannelI_StemJ]] | 3 (1 / 0 / 3) |
| `num_button_parameterslots` | The number of button parameter slots available. | read-only integer | missing | *effect units*; in [EffectRack1_EffectUnitN_EffectM], [EqualizerRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI_StemJ]_Effect1] | 3 (0 / 0 / 3) |
| `num_effectunits` | The number of EffectUnits in this rack | read-only integer | missing | *effect units*; in [EffectRack1], [EqualizerRack1], [QuickEffectRack1] | 3 (0 / 0 / 3) |
| `num_parameterslots` | The number of parameter slots available. | read-only integer | missing | *effect units*; in [EffectRack1_EffectUnitN_EffectM], [EqualizerRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI_StemJ]_Effect1] | 3 (0 / 0 / 3) |
| `group_[SamplerN]_enable` | Whether or not this EffectChain applies to Sampler J | binary | missing | *effect units* | 2 (0 / 0 / 2) |
| `next_chain_preset` | Cycle to the next EffectChain preset after the currently loaded preset. | binary | missing | *effect units*; in [EffectRack1_EffectUnitN], [EqualizerRack1_[ChannelI]], [QuickEffectRack1_[ChannelI]], [QuickEffectRack1_[ChannelI_StemJ]] | 1 (0 / 0 / 1) |
| `loaded_effect` | 0-based index of the currently loaded effect preset, including the empty/passthrough… | integer | missing | *effect units*; in [EffectRack1_EffectUnitN_EffectM], [EqualizerRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI]_Effect1], [QuickEffectRack1_[ChannelI_StemJ]_Effect1] | – |
| `num_chain_presets` | The number of effect chain presets available in this EffectUnit, including the… | read-only integer | missing | *effect units*; in [EffectRack1_EffectUnitN], [EqualizerRack1_[ChannelI]], [QuickEffectRack1_[ChannelI]], [QuickEffectRack1_[ChannelI_StemJ]] | – |
| `prev_chain_preset` | Cycle to the previous EffectChain preset before the currently loaded preset. | binary | missing | *effect units*; in [EffectRack1_EffectUnitN], [EqualizerRack1_[ChannelI]], [QuickEffectRack1_[ChannelI]], [QuickEffectRack1_[ChannelI_StemJ]] | – |
| `chain_selector` | Select EffectChain preset. | value | missing | *effect units*; deprecated; in [EffectRack1_EffectUnitN], [EqualizerRack1_[ChannelI]], [QuickEffectRack1_[ChannelI]] | 9 (0 / 0 / 9) |
| `next_chain` | Cycle to the next EffectChain preset after the currently loaded preset. | value | missing | *effect units*; deprecated; in [EffectRack1_EffectUnitN], [EqualizerRack1_[ChannelI]], [QuickEffectRack1_[ChannelI]] | 8 (4 / 1 / 4) |
| `prev_chain` | Cycle to the next EffectChain preset after the currently loaded preset. | value | missing | *effect units*; deprecated; in [EffectRack1_EffectUnitN], [EqualizerRack1_[ChannelI]], [QuickEffectRack1_[ChannelI]] | 6 (2 / 0 / 4) |
| `show` | Show the Effect Rack | binary | missing | *effect units*; deprecated | 6 (4 / 0 / 2) |

### Inputs: [MicrophoneN], [AuxiliaryN]

Groups: `[AuxiliaryN]`, `[ChannelN]`, `[MicrophoneN]`, `[PreviewDeckN]`, `[SamplerN]`.

| Control | What it does | Type | earshot | Equivalent / needs | Used by (XML in / out / JS) |
|---|---|---|---|---|---|
| `pfl` † | Toggles headphone cueing (PFL). | binary | missing | *microphone and aux inputs*; in [AuxiliaryN], [MicrophoneN] | 77 (0 / 0 / 77) |
| `volume` ± † | Adjusts the channel volume fader. | value | missing | *microphone and aux inputs*; in [AuxiliaryN], [MicrophoneN] | 74 (2 / 0 / 72) |
| `pregain` ± † | Adjusts the gain of the input. | 0.0..1.0..4.0 | missing | *microphone and aux inputs*; in [AuxiliaryN], [MicrophoneN] | 69 (1 / 0 / 69) |
| `vu_meter` † | Outputs the current instantaneous channel volume | value | missing | *microphone and aux inputs*; in [AuxiliaryN], [MicrophoneN] | 38 (0 / 0 / 38) |
| `peak_indicator` † | Indicates when the signal is clipping (too loud for the hardware and is being distorted) | binary | missing | *microphone and aux inputs*; in [AuxiliaryN], [MicrophoneN] | 24 (0 / 0 / 24) |
| `vu_meter_left` † | Outputs the current instantaneous deck volume for the left channel | value | missing | *microphone and aux inputs*; in [AuxiliaryN], [MicrophoneN] | 20 (0 / 0 / 20) |
| `vu_meter_right` † | Outputs the current instantaneous deck volume for the right channel | value | missing | *microphone and aux inputs*; in [AuxiliaryN], [MicrophoneN] | 20 (0 / 0 / 20) |
| `orientation` † | Set channel orientation for the crossfader. | enum | missing | *microphone and aux inputs* | 15 (0 / 0 / 15) |
| `mute` † | Mutes the channel | binary | missing | *microphone and aux inputs*; in [AuxiliaryN], [MicrophoneN] | 8 (0 / 0 / 8) |
| `talkover` | Hold value at 1 to mix channel input into the main output. | binary | missing | *microphone and aux inputs*; in [AuxiliaryN], [MicrophoneN] | 8 (1 / 1 / 7) |
| `peak_indicator_left` † | Indicates when the signal is clipping (too loud for the hardware and is being distorted)… | binary | missing | *microphone and aux inputs*; in [AuxiliaryN], [MicrophoneN] | 4 (0 / 0 / 4) |
| `peak_indicator_right` † | Indicates when the signal is clipping (too loud for the hardware and is being distorted)… | binary | missing | *microphone and aux inputs*; in [AuxiliaryN], [MicrophoneN] | 4 (0 / 0 / 4) |
| `input_configured` | 1 if there is input is configured for this channel, 0 if not. | read-only binary | missing | *microphone and aux inputs*; in [AuxiliaryN], [ChannelN], [MicrophoneN] | 1 (0 / 0 / 1) |
| `main_mix` | Hold value at 1 to mix channel input into the main output. | binary | missing | *microphone and aux inputs*; in [AuxiliaryN], [MicrophoneN] | 1 (0 / 0 / 1) |
| `orientation_center` | Assign channel to the center of the crossfader. | value | missing | *microphone and aux inputs*; in [AuxiliaryN], [ChannelN], [PreviewDeckN], [SamplerN] | – |
| `orientation_left` | Assign channel to the left side of the crossfader. | value | missing | *microphone and aux inputs*; in [AuxiliaryN], [ChannelN], [PreviewDeckN], [SamplerN] | – |
| `orientation_right` | Assign channel to the right side of the crossfader. | value | missing | *microphone and aux inputs*; in [AuxiliaryN], [ChannelN], [PreviewDeckN], [SamplerN] | – |
| `enabled` † | 1 if a channel input is enabled, 0 if not. | binary | missing | *microphone and aux inputs*; deprecated; in [AuxiliaryN], [MicrophoneN] | 48 (0 / 0 / 48) |
| `orientation` † | (No description) | value | missing | *microphone and aux inputs*; deprecated | 15 (0 / 0 / 15) |
| `VuMeter` † | Outputs the current instantaneous channel volume | value | missing | *microphone and aux inputs*; deprecated; in [AuxiliaryN], [MicrophoneN] | 10 (0 / 0 / 10) |
| `VuMeterN` † | Outputs the current instantaneous channel volume | value | missing | *microphone and aux inputs*; deprecated; in [AuxiliaryN], [MicrophoneN] | 10 (0 / 0 / 10) |
| `master` | Hold value at 1 to mix channel input into the main output. | binary | missing | *microphone and aux inputs*; deprecated; in [AuxiliaryN], [MicrophoneN] | 4 (0 / 0 / 4) |
| `PeakIndicator` † | Indicates when the signal is clipping (too loud for the hardware and is being distorted) | binary | missing | *microphone and aux inputs*; deprecated; in [AuxiliaryN], [MicrophoneN] | 2 (0 / 0 / 2) |
| `PeakIndicatorN` † | Indicates when the signal is clipping (too loud for the hardware and is being distorted)… | binary | missing | *microphone and aux inputs*; deprecated; in [AuxiliaryN], [MicrophoneN] | 2 (0 / 0 / 2) |

### [AutoDJ]

Groups: `[AutoDJ]`.

| Control | What it does | Type | earshot | Equivalent / needs | Used by (XML in / out / JS) |
|---|---|---|---|---|---|
| `enabled` † | Turns Auto DJ on or off. | binary | missing | *Auto DJ* | 53 (6 / 5 / 48) |
| `skip_next` | Skips the next track in the Auto DJ playlist. | binary | missing | *Auto DJ* | 5 (0 / 0 / 5) |
| `fade_now` | Triggers the transition to the next track. | binary | missing | *Auto DJ* | 3 (1 / 0 / 2) |
| `shuffle_playlist` | Shuffles the content of the Auto DJ playlist. | binary | missing | *Auto DJ* | 1 (0 / 0 / 1) |
| `add_random_track` | Adds a random track to the Auto DJ queue. | binary | missing | *Auto DJ* | – |

### [Sampler] (banks)

Groups: `[Sampler]`.

| Control | What it does | Type | earshot | Equivalent / needs | Used by (XML in / out / JS) |
|---|---|---|---|---|---|
| `LoadSamplerBank` | Load saved sampler configuration file and add tracks to the available samplers. | binary | missing | *sampler decks* | – |
| `SaveSamplerBank` | Save sampler configuration. | binary | missing | *sampler decks* | – |

### [Recording]

Groups: `[Recording]`.

| Control | What it does | Type | earshot | Equivalent / needs | Used by (XML in / out / JS) |
|---|---|---|---|---|---|
| `status` † | Indicates whether Mixxx is currently recording. | enum | missing | *recording* | 11 (0 / 0 / 11) |
| `toggle_recording` | Turns recording on or off. | binary | missing | *recording* | 10 (1 / 0 / 9) |

### [VinylControl]

Groups: `[VinylControl]`.

| Control | What it does | Type | earshot | Equivalent / needs | Used by (XML in / out / JS) |
|---|---|---|---|---|---|
| `gain` † | Allows to amplify the “phono” level of attached turntables to “line” level. | binary | missing | *timecode vinyl* | 14 (0 / 0 / 14) |
| `Toggle` | Moves control by a vinyl control signal from one deck to another if using the single… | binary | missing | *timecode vinyl* | – |
| `show_vinylcontrol` † | Toggle the vinyl control section in skins. | binary | missing | *timecode vinyl*; deprecated | 1 (0 / 0 / 1) |

### Stems: [ChannelN_StemM]

Groups: `[ChannelN_StemM]`.

| Control | What it does | Type | earshot | Equivalent / needs | Used by (XML in / out / JS) |
|---|---|---|---|---|---|
| `color` | The color for the STEM M on the deck. | read-only colour | missing | *stems* | 1 (0 / 0 / 1) |

### [Waveform]

Groups: `[Waveform]`.

| Control | What it does | Type | earshot | Equivalent / needs | Used by (XML in / out / JS) |
|---|---|---|---|---|---|
| `stem_split_tracks` | Toggles between stacked (split tracks) and overlapping stem waveform display modes. | enum | missing | *stems* | – |

### [Skin]

Groups: `[Skin]`.

| Control | What it does | Type | earshot | Equivalent / needs | Used by (XML in / out / JS) |
|---|---|---|---|---|---|
| `show_maximized_library` | Toggle maximized view of library section of the user interface. | binary | n/a |  | 26 (11 / 4 / 16) |
| `show_samplers` † | Toggle the display of sampler banks in the user interface. | binary | n/a |  | 8 (0 / 0 / 8) |
| `show_effectrack` | Toggle the display of the effect rack in the user interface. | binary | n/a |  | 1 (0 / 0 / 1) |
| `show_library_coverart` | Toggle the display of cover art in the library section of the user interface. | binary | n/a |  | 1 (0 / 0 / 1) |
| `show_vinylcontrol` † | Toggle the vinyl control section in the user interface. | binary | n/a |  | 1 (0 / 0 / 1) |
| `show_stem_controls` | Toggle the STEM control section. | binary | n/a |  | – |

### [Controls]

Groups: `[Controls]`.

| Control | What it does | Type | earshot | Equivalent / needs | Used by (XML in / out / JS) |
|---|---|---|---|---|---|
| `ShowDurationRemaining` | Represents the current state of the remaining time duration display of the loaded track. | enum | n/a |  | 3 (0 / 0 / 3) |
| `touch_shift` | Once enabled, all touch tab events are interpreted as right click. | binary | n/a |  | 3 (0 / 0 / 3) |
| `AutoHotcueColors` | If enabled, colors will be assigned to newly created hotcues automatically. | binary | n/a |  | – |

### [Shoutcast]

Groups: `[Shoutcast]`.

| Control | What it does | Type | earshot | Equivalent / needs | Used by (XML in / out / JS) |
|---|---|---|---|---|---|
| `enabled` † | Shows if live Internet broadcasting is enabled. | value | n/a |  | 48 (0 / 0 / 48) |
| `status` † | This control displays whether broadcasting connection to Shoutcast server was… | binary | n/a |  | 11 (0 / 0 / 11) |

### [Samplers] (deprecated)

Groups: `[Samplers]`.

| Control | What it does | Type | earshot | Equivalent / needs | Used by (XML in / out / JS) |
|---|---|---|---|---|---|
| `show_samplers` † | (No description) | binary | n/a | deprecated | 11 (3 / 0 / 8) |

<!-- END GENERATED -->
