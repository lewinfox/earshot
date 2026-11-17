# What controllers send and expect: findings from Mixxx's mappings

**Summary:** messages are shared within a product family, not across a whole
maker. Inputs come in three kinds (button, absolute, relative), and jogs use
one of three encodings. Outputs are mostly on/off lights on the button's own
message. So a mapping can be a short list of "message → input, kind" lines, with
a family base file and small per-model overrides.

Measured on 2026-11-11 over the 142 MIDI mappings in
`/usr/share/mixxx/controllers` (Mixxx 2.5). The throwaway scripts were not kept;
`tools/` has the older ones.

## 1. One file per maker? No. One per family, yes.

Each family below uses the same messages for the same physical controls. Other
models from the same maker don't.

| Family | Same messages as each other | Not the same as |
|---|---|---|
| Pioneer DDJ (400, FLX4, 200, SB, SB2, SB3, SX) | Play `90 0B`, cue `90 0C`, sync `90 58`, shift `9n 3F`, pads `97+`, crossfader `B6 1F`, tempo `B0 00`/`B0 20`, EQ hi `B0 07`/`B0 27`, jog side `B0 21` | Pioneer CDJs (play `90 00`): 11 % shared |
| Pioneer CDJ (350, 850, 2000) | 94–100 % | DDJs |
| Hercules DJControl (Inpulse 200/300/500, MIX, Starlight) | 91–98 % of messages | Older Hercules (DJ Console, Instinct): ~1 % shared |
| inMusic (newer Numark, Denon MC4000/MC7000) | Sync `90 02`, crossfader `BF 08`, pads `94+`; 81–98 % shared, *across brands* | Older Numark (play `90 3B`) |
| Behringer BCD2000/3000 | 100 % | Behringer CMD range |

- **DDJ-400 vs DDJ-FLX4:** 195 of 201 messages shared, 99 % with the same
  meaning. Effectively one mapping.
- **DDJ-400 vs SB3/SX:** 76–81 % of messages shared. The physical controls
  agree. The pad modes differ (the 400's "beat loop" pad is the SB3's "slicer"
  pad), and 4-deck models use channels the 400 uses for shift (`9A 00` is
  "deck 2 pad 1 + shift" on the 400, "deck 4 pad 1" on the SB3).
- **Whole-maker agreement is low:** Hercules 33 %, Numark 44 %, Pioneer 61 % of
  shared messages mean the same thing.

**So:** a base file per family (`pioneer-ddj.txt`), plus a short file per model
that overrides or adds lines. The DDJ-FLX4 file would be a handful of lines on
top of the DDJ-400's.

## 2. Input kinds

What the 142 mappings receive:

| Message | Uses | Mappings |
|---|---|---|
| Note on / off | 12,620 | 129 |
| CC | 4,184 | 137 |
| Pitch bend | 87 | 27 |
| Program change, sysex, other | 44 | 8 |

Every control is one of three kinds:

1. **Button.** A note: pressed (value > 0) or released (0, or a note-off
   message). A few send a CC instead (Mixxx's `button`, `switch` options).
2. **Absolute.** A knob or fader position.
   - 7-bit: one CC, 0–127.
   - 14-bit: two CCs, coarse and fine, the fine one's number 32 higher (16
     mappings, 74 pairs). The DDJ-400 does this for every knob and fader.
   - Pitch bend: 14 bits in one message (rare).
   - Sometimes upside down (Mixxx's `invert`, 22 mappings; the DDJ-400's tempo
     fader).
3. **Relative.** "Moved n steps": jog wheels, browse encoders. Needs an
   encoding (below) and, for jogs, ticks per turn.

That's it. Everything Mixxx's XML options do is one of these, plus soft
takeover (core behaviour, not mapping).

## 3. Jog wheel encodings

Of 99 mappings with a jog, read from their scripts:

| Encoding | Still | +1 | −1 | Mappings | Examples |
|---|---|---|---|---|---|
| **Centre 64** | 64 | 65 | 63 | ~52 | Pioneer DDJ, Denon MC6000 |
| **Two's complement** | 0 | 1 | 127 | ~35 | Hercules, newer Numark, Mixxx's `JogWheelBasic` |
| **Absolute position** | (same value) | value goes up | value goes down | ~8 | Numark NS7 (motorised platter): earshot takes the difference from the last value |
| Pitch bend | | | | 2 | iPad apps |

A "sign bit" encoding (bit 6 is direction) is described in MIDI docs but no
mapping here uses it.

**Ticks per turn** varies a lot: 128 (27 scripts), 720 (DDJ-400), 1024, 600,
37056, etc. It's the second number in the script's `engine.scratchEnable(...)`,
a plain number in about 80 of 97 scripts. Readable by a tool; the rest by hand.

Jog **touch** is always a button (note).

## 4. Outputs (lights)

116 of 142 mappings send something back: 61 through XML `<output>`s (2,290 of
them), 110 from their scripts.

**87 % of XML outputs use the same message as an input**: the light on a button
is lit by sending the button's own message back. So most outputs need no
mapping line of their own: "light input x" is enough.

Output kinds:

1. **On/off light.** Send a value for on (`7F` nearly always) and one for off
   (`00`). Lit while a control is within a range (`minimum` 0.5 for most
   buttons). The DDJ-400's 112 XML outputs are all this.
2. **Two brightnesses.** Same, with a non-zero off value (`01`, `64`): dim when
   off, bright when on. ~200 outputs.
3. **Level.** A VU meter or similar. Either one CC whose value is the level (the
   DDJ-400: `B0 02` / `B1 02`, from its script) or a row of on/off lights, each
   on above a threshold.
4. **Colour.** The value picks a colour from the device's palette, e.g. hot cue
   pads in the cue's colour. 14 mappings. Needs a palette table per device.
5. **Displays.** Text or numbers by sysex. Rare and model-specific; later.

**Blinking** (74 scripts use timers, mostly for it) is behaviour: the core
blinks a light by switching it on and off. Not a mapping concern.

The most-lit things: hot cue set (435), playing (186), headphone cue (120),
effect on (114), VU meter (111), loop on (164 across sizes), cue (80), track
loaded (72).

## What this means for earshot's mapping files

A line says which message is which input, and what kind:

```
in  90 0B        deck1 play                 button
in  B0 13 B0 33  deck1 volume               absolute14
in  B0 00 B0 20  deck1 tempo                absolute14 invert
in  B0 22        deck1 jog_top              relative centre64 ticks=720
in  B6 40        library browse             relative twos
out 90 0B        deck1 playing              on=7F off=00
out B0 02        deck1 meter                level
```

- One file per family, one small file per model on top.
- Outputs default to the input's own message, with `7F`/`00`, so most need no
  line.
- Everything else (scratch, shift, pad modes, blinking, soft takeover) is the
  core's.
