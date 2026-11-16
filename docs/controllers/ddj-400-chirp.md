# The DDJ-400 headphone chirp

**Summary:** asking a DDJ-400 for its knob positions within a moment of its sound
card starting to play can leave its headphone mix chirping until it's unplugged.
earshot now waits until audio has been playing for 2 seconds before it asks.

Fixed in `a0b0716` and `ff1048b`, merged to `develop` in `41b219c` (PR #36).

## What it looked and sounded like

- A high, thin chirp in the headphones only, in time with the kick. The master
  output was clean.
- Sometimes the headphone level or cue/master knob did nothing, or the headphones
  were very loud.
- The DDJ ignored earshot's level meter messages.
- It survived restarting earshot. Only unplugging the DDJ cleared it.
- Once, after a bad run, Mixxx couldn't see the DDJ at all.
- Not every start: some runs were fine, which made it look random.

## How it happened

1. **Sun 15 Nov, 22:32** — Commit `e259fba` started sending the DDJ-400 its
   "report your positions" SysEx (`F0 00 40 05 00 00 02 06 00 03 01 F7`) whenever
   MIDI connected, as Mixxx's mapping does. It seemed harmless: Mixxx sends the
   same bytes.
2. **Same commit** — The hidden assumption: *when* it's sent doesn't matter.
   earshot opens the DDJ's sound card first, then MIDI, so the query arrived about
   1 ms after the card started playing. Mixxx sends it *before* its audio starts.
3. **Mon 16 Nov** — The chirp appeared. The DDJ does its own headphone mix
   (earshot's headphone channels are silent unless a deck is cued), so a confused
   DDJ mixes badly while earshot's output is fine.
4. **Now** — Removing the query at connect gave 6 clean starts out of 6. Sending
   the same query from a button mid-run was harmless. So it's the timing, not the
   message.

**Gotcha:** earshot's audio was bit-identical in good and bad runs. The fault was
inside the controller, so nothing in earshot's own output could show it.

## What we ruled out, and how

- **earshot's audio.** Debug mode recorded exactly what went to the sound card.
  Good and bad runs, lined up by track position, correlated at 1.000000. Same card
  setup in both (S16_LE, 4 channels, 44 100 Hz, period 256, buffer 512), no
  underruns.
- **The engine.** A separate review of the engine found no fault.
- **"The cue is twice as loud as master."** Real, but not a fault: cue is taken
  before the crossfader, and the crossfader sat at 0.26 with the constant-power
  curve, which puts master 5.2 dB down. Measured: +5.25 dB.
- **Level meters (PR #35).** Suspected for a while. The chirp happened on
  `develop` without them.
- **Real-time priority.** "Failed to promote audio thread to real-time priority"
  came from rtkit, which had gone into "recovering from system lockup" mode and
  was refusing everyone. `sudo systemctl restart rtkit-daemon` fixed that. It
  wasn't the cause.

## What the DDJ-400 expects

- **Pioneer's MIDI list** (from the `pestrela/ddj` repo): channel 16 (`9F`/`BF`)
  holds the DDJ's own settings, such as fader start (`9F 42`), crossfader reverse
  (`9F 43`) and level meter mode (`9F 4B`). earshot never sends on channel 16.
  `9F 00`/`9F 01` light the "track loaded" animation (issue #37). `Bn 02` is a
  channel level meter from the computer.
- **What Mixxx does** (from `mixxx --controller-debug`): zero the meters, send
  `9F 00 7F` / `9F 01 7F`, set the loop lights, send the report SysEx, *then*
  start audio. The DDJ answers within 30 ms with every control's position, plus
  `9F 47 7F` and `9F 49 7F`, which Pioneer doesn't document.
- **rekordbox** sends its own handshake. A forum thread asked people not to post
  those bytes, since they get around rekordbox's licensing, so we haven't looked.

## What earshot does now

- **No query at connect.** Removed in `a0b0716`.
- **`init_controller()`** (`gui/src/core/controller_init.rs`, called every frame
  from `gui/src/ui/app.rs`) asks once audio has been playing for `SETTLE` (2 s).
  Any reopen of audio starts the wait again: the controller arriving or leaving,
  taking the controller's audio from PipeWire, Settings › Audio › Restart, or the
  audio watchdog. Each MIDI connect asks again once.
- **The lights thread sends the query** and logs `[midi] sent the position query
  to N DDJ-400(s)`.
- **Debug mode's "Ask DDJ for positions" button** sends it by hand.
- **Start-up logging** tagged `[init]`: the card found and its state from
  `/proc/asound`, PipeWire handing the card over (and how long it took), each
  stream open and close, the config chosen, MIDI ports opened, and who asked for
  an audio reopen. Underruns are logged: the first 10, then every 100th.

2 s is a guess that has worked so far, not a measured limit.

## If it comes back

1. Unplug and replug the DDJ to clear it.
2. Run a debug build, reproduce it, and export from Debug mode. The zip has the
   output audio, the log and the card setup.
3. In `log.txt`, compare the time of `[midi] sent the position query` with the
   last `[init]` stream open. Under 2 s apart means `SETTLE` was skipped.
4. Try a longer `SETTLE`, or sending the query before audio opens, as Mixxx does.
