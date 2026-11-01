# Adding a controller earshot doesn't know

**Summary:** when someone plugs in a controller with no mapping, earshot should
work out what it is, then walk them through pressing its controls to build a
mapping. It guesses as much as it can from Mixxx's mappings for similar
controllers, so the user only confirms. The result is a mapping file in our own
format, plus an optional GitHub issue so we can add the controller to earshot.
All of this lives in a new third mode, **Configure**, next to Perform and
Prepare.

This sits alongside the Mixxx plan in [README.md](README.md). That plan runs
mappings that already exist. This doc covers controllers that have none, or
whose mapping the user wants to change.

## The layers

1. **Detection:** what's plugged in?
2. **Knowledge:** what do we already know about this controller, or ones like it?
3. **Capture:** record what each control sends.
4. **Mapping:** link what was captured to earshot controls, and save it.
5. **Sharing:** a GitHub issue for us to add built-in support.
6. **LEDs:** later, as a separate project.

## 1. Detection

A MIDI controller's messages don't say what model it is. The USB device does.
Try these in order, most reliable first, and keep every answer:

| # | Method | What it gives | Reliability |
|---|---|---|---|
| 1 | USB vendor and product IDs | Two numbers fixed per model, e.g. `2b73:xxxx` for Pioneer DJ | Exact. Linux: `/proc/asound/cardN/usbid`, linked to the MIDI port through the ALSA card number |
| 2 | MIDI identity request | Maker ID, model family, model, firmware version | Exact when it answers; many DJ controllers don't |
| 3 | Port name | The product name the device reports, e.g. "DDJ-400 MIDI 1" | Readable, but the wording can change between firmware versions and operating systems |
| 4 | Maker only | From the vendor ID or the identity reply's maker ID | Enough to pick a family to guess from (layer 2) |

- `midir` only gives the port name (`connect_all` in `gui/src/core/midi.rs`).
  Methods 1 and 2 need a little ALSA work and a sysex send.
- Mixxx's MIDI mappings match devices by name only. Vendor and product IDs are
  a field we add.
- The result is a **device identity**: the IDs, the identity reply, the port
  name, and the maker. Mapping files and GitHub issues both carry it.
- Matching order when a device appears: an earshot mapping by IDs, then by name,
  then a Mixxx mapping by name, then "unknown" (offer Configure).

## 2. Knowledge: reuse what Mixxx knows

Mixxx installs 159 mappings in `/usr/share/mixxx/controllers`, about 20 of them
for Pioneer. `docs/controllers/tools/` already parses them. Build an index from
them at runtime (never ship a copy: GPL, see
[mixxx-mappings.md](mixxx-mappings.md#our-own-format)):

- **Per maker or family:** for each (status, data1) pair, the `[Group] key` it
  maps to in each mapping, and how many mappings agree. For example: "channel 1,
  note 0x36 is jog touch on 5 other DDJs".
- **Behaviour hints from the options:** 14-bit pairs, relative encoders,
  `<Script-Binding/>` functions whose names say what they do (`jogTurn`,
  `jogTouch`, `shiftPressed`).
- **Startup messages:** the sysex each script sends in `init`. Some ask the
  controller to report every knob and fader's position. The DDJ-400's script
  does (commented "query the controller for current control positions"). 32
  scripts send sysex; a tool needs to sort out which messages are position
  requests.
- **LED outputs:** the `<output>` entries and `midi.sendShortMsg` calls, for
  layer 6.

**Browsing it in earshot:** Configure gets a read-only "Known controllers"
view. It lists every mapping found, grouped by maker. Each one shows its
bindings, which earshot controls they reach, and what's missing (the phase 0
numbers from [README.md](README.md)). It's useful on its own, and it's what the
guesses in layer 3 come from.

## 3. Capture

### What can't be done

There is no standard way to ask a MIDI controller to list its controls. USB
MIDI device info lists only ports. MIDI 2.0 can describe controls, but no DJ
controller supports it. So capture has to watch the user move things. Two
partial exceptions:

- **Position requests** (above) report every knob and fader in one go. They
  don't cover buttons, because a button has no position until pressed.
- **HID controllers** (mainly Native Instruments) describe every input field
  with its size and range, but not what it does. That's a full list to label.
  HID support is phase 6 of the main roadmap; capture for HID waits for it.

### Guided capture (the default)

earshot asks for one control at a time from a fixed list of core functions. The
answer is labelled automatically.

1. **Detect** the device (layer 1).
2. **Send the position request**, if the knowledge index has one for this
   family. Match the replies against guesses, and pre-fill what fits.
3. **Pre-fill guesses** from sibling mappings for everything else.
4. **Walk the list**, deck by deck:
   - Already guessed: "Press Play on deck 1". If the message matches the guess,
     tick it and move on.
   - Not guessed: "Press Play on deck 1, or Skip".
   - The user can always skip. A controller without a control is normal.
5. **Shift layer:** "Hold Shift and press Play on deck 1". If the message
   differs from the unshifted one, earshot learns how Shift works on this device
   (another channel, or other note numbers) and predicts the rest of the shift
   layer.
6. **Test:** a live view where every control lights up its earshot control on
   screen as it's moved. Wrong ones are re-captured with one click.

The core list, roughly in order (earshot controls from the registry):

- **Per deck:** play, cue, sync, tempo fader, jog turn, jog touch, load, hot
  cues 1–8, loop in/out, beat loop, beat jump back/forward, pfl.
- **Mixer per channel:** volume fader, trim, EQ high/mid/low, filter.
- **Global:** crossfader, master volume, headphone volume and mix, browse turn
  and press, Shift.

### Free capture

For anything the list doesn't cover: pads in other modes, FX, extra buttons.

- A running list of every distinct (status, data1) seen, with its behaviour
  (below), when it was last seen, and its value.
- Each row can be bound to any earshot control, or just annotated in plain text
  ("FX 1 knob, not used yet"). Annotations are saved in the file and sent with
  the GitHub issue, so a later version can map them.
- Rows highlight when touched, so "which row is this knob?" is answered by
  turning it.

### Working out what kind of control it is

Done from the messages alone, for both modes:

| Kind | How to tell |
|---|---|
| Button | Note on, then note off or note on with velocity 0 |
| Toggle-style button | Note on only, value flips between presses |
| Knob or fader | CC sweeping 0–127 |
| 14-bit fader | Two CCs 32 apart, always arriving together |
| Jog wheel or endless encoder | CC with values near 1/127 or 63/65, not sweeping. The centre tells the encoding (around 0, around 64, or a sign bit) |
| Pitch bend | Status `0xE_`, 14 bits |
| Touch strip / jog touch | A note that stays on while touched, often next to a jog CC |

## 4. Mapping files

The format is the one already planned in
[mixxx-mappings.md](mixxx-mappings.md#our-own-format): keymap-style lines,
earshot control names, one file per device in
`~/.local/share/earshot/controllers/`. Capture adds:

- **Identity lines:** `usb 2b73:xxxx`, `identity <reply bytes>`, and the port
  name in `device`. Matching uses them in that order.
- **Behaviour options** on `in` lines, from the table above (`14bit`,
  `relative=offset64`, …). These are the same option maths phase 1 needs for
  Mixxx's XML.
- **`note` lines** for free-capture annotations with no binding:
  `note 0xB4 0x02 "FX 1 knob"`.
- **Provenance:** a comment header with the earshot version, the date, and
  which guesses came from which Mixxx mapping.

If the controller has a Mixxx mapping, the file keeps the `mixxx` line, and
captured changes become overrides on top of it. Capture never edits Mixxx's
files.

## 5. Sharing: a GitHub issue

- A "Send to earshot" button opens a prefilled
  `github.com/lewinfox/earshot/issues/new?title=…&body=…` link in the browser.
  No login or token in earshot.
- Links are limited to about 8 KB, so the body has a summary: the device
  identity, how many controls were captured and bound, the annotations, and
  which Mixxx mapping (if any) was the base.
- The full mapping file is saved to a known path. The body asks the user to
  attach it, and earshot shows the path.
- A GitHub issue template (`.github/ISSUE_TEMPLATE/controller.yml`) gives these
  issues a fixed shape and a `controller` label.

Built-in support then means adding the file to the repo. Built-in mappings load
like user ones, and a user's file wins over the built-in one.

## 6. LEDs (later)

Capture only records what the controller sends. Lighting LEDs means sending
messages to it, which needs a separate flow:

- Use Mixxx's `<output>` entries and scripts for siblings to guess which message
  lights what. It's often the button's own note, but not always.
- Send each guess and ask "Did the Play light come on?".
- Some controllers need a startup sysex before LEDs work. The knowledge index
  already collects those.

See [feedback.md](feedback.md) for how earshot sends feedback once it knows what
to send.

## Configure mode

A third mode, next to Perform and Prepare (`Mode` in `gui/src/core/view.rs`).
Capture needs the whole screen, and it shouldn't be one click from the decks
during a set. Its tabs:

- **Devices:** what's connected, how each was identified (layer 1), and which
  mapping it uses. "Configure this controller" starts capture.
- **Capture:** guided and free capture, and the test view.
- **Known controllers:** the browsable Mixxx index.
- **Keymap:** the computer keyboard bindings could move here from Settings
  later, since it's the same idea.

## Order of work

Each step works on its own:

1. **Detection:** USB IDs and port name, shown in a Devices tab. Identity
   request after.
2. **Free capture** with control-kind detection, writing `in` and `note` lines.
   This is phase 0's "press everything" log with a UI, and it's useful without
   any guessing.
3. **Guided capture** with the core list, Shift, and the test view.
4. **Knowledge index** and guesses, then position requests.
5. **Known controllers** browser.
6. **GitHub issue** link and template.
7. **LEDs.**

Steps 2 and 3 need the option maths and the line format from phase 1 of the
main roadmap. Build those first, or alongside.

**The gotcha:** a guess that's wrong is worse than no guess. If Play and Cue are
swapped, the whole mapping feels broken. Every guess has to be confirmed by
pressing the control, and anything unconfirmed is saved as a `note`, not a
binding.
