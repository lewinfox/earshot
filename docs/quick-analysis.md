# Quick analysis: a grid and a key in a fraction of a second

Status: **prototype, being evaluated.** Code: `audio/src/quick/` (the analysis),
`gui/src/core/player.rs` (`grid_for_file`: where the app uses it) and
`gui/examples/quick_eval.rs` (the evaluation).

## Why

earshot's beat grids come from the Python worker. Static analysis is a fast tempo pass plus
Beat This! on five 15 s excerpts, about 3–5 s per track; dynamic analysis is Beat This! over
the whole track, about 15–30 s. Until that's done, a track on a deck plays on a placeholder:
a steady grid at its BPM tag, with its first beat at 0 s. That placeholder is usually in
the wrong place, and it's no use at all for a track without a BPM tag. earshot has no key
analysis at all; keys come only from tags.

[ClubCraft](https://github.com/beneaze/ClubCraft), a Rust DJ app, claims every track is
analysed "within about a second". Its code bears that out. Its beat grid and key come from
classical signal processing, with no model, at 0.1–0.5 s for a 5-minute track. The cost is
generality: one steady tempo, a prior around 124 BPM, and a downbeat on the first beat
with music in it. That suits steady dance music and nothing else. earshot keeps Beat This!
for everything this can't do (drifting tempos, live drummers, tempo-lock, odd downbeats),
but the quick pass is worth having:

1. **On load.** A track that hasn't been analysed plays on the quick grid instead of the
   tag placeholder, from the moment it's loaded. The worker's analysis still runs and
   replaces it.
2. **Later, if the evaluation supports it.**
   - A key for every track, stored as analysis below the tag in precedence.
   - The quick grid as the static analysis's first pass, with Beat This! only confirming it.
   - A drift flag from how far the quick grid's segment phases stray.
   - Instant grids and keys for a whole folder on import.

## What it does

`quick::analyse(&TrackAudio, tag_bpm)` returns a `Constant` grid with a confidence, and a
key with a score. The two run side by side, and the STFTs are spread over a few threads.
The module docs (`grid.rs`, `key.rs`) have the details.

- **Grid.**
  1. A SuperFlux-style onset envelope, with the band below 200 Hz weighted double.
  2. Autocorrelation tempo candidates, with a log-Gaussian prior around 124 BPM.
  3. The grid fit. The envelope is folded onto one beat, segment by segment. A line is
     fitted through the segments' phases (slope: tempo correction; intercept: phase),
     using weighted least squares that drops outliers. Doubled angles mean segments
     locked onto the off-beat don't spoil it.
  4. A check on whether the grid sits on the beat or between beats, from the kick, clap
     and hat bands.
  5. Snapping to a whole or half BPM.
  6. The first beat with music in it is the 1.
- **Key.**
  1. A 16k-point STFT, median-filtered over time to keep the harmonic part.
  2. A tuning estimate, then a chroma from 40 Hz to 2.1 kHz.
  3. Correlation with a blend of the KeyFinder (Sha'ath) and EDMA (Faraldo) key profiles.
- **On load** (`grid_for_file`):
  1. A tempo-lock sidecar still wins.
  2. Otherwise the quick grid, if its confidence is at least 1.0. The track's BPM tag is
     the hint.
  3. Otherwise the tag placeholder, as before.

  The deck's grid editor says "from quick grid". This adds roughly 0.1–0.4 s to loading a
  track that hasn't been analysed; analysed tracks are untouched.

### Changes from ClubCraft

- **Left out:**
  - its bar-phase evidence features, used only when no beat has music in it
  - its structure, cue, energy and stem code
  - rayon (`std::thread::scope` does the STFTs)
- **Sample rates.** earshot doesn't resample, so the STFT sizes are kept at about 23 ms
  (grid) and 370 ms (key) frames up to 48 kHz. ClubCraft resamples to 44.1 kHz.
- **A tag hint.** `GridOptions::around(bpm)` searches ±40% around the track's BPM tag with
  a narrow prior. Without it, a 174 BPM drum & bass track locks onto 116, two-thirds of
  its tempo. The worker's tempo pass has the same trap, and the port of ClubCraft's
  dance-music default falls into it on the demo track. The evaluation runs every track both with and
  without the hint.
- **The 3 ms offset.** The grid is shifted 3 ms earlier, which ClubCraft calibrated against
  DJ.Studio and Mixed In Key. earshot's own grids are moved to where the kick starts
  (`worker/earshot_worker/snap.py`). The evaluation reports the typical offset against
  earshot's grids, so this constant can be set from real tracks.

### Licence

ClubCraft is MIT or Apache-2.0. The port is used under MIT; the notice is in
`audio/src/quick/LICENSE-ClubCraft`, and the module docs credit it. ClubCraft's own
clean-room policy covers its interop with other apps, not this analysis code.

## Known limits

- **One tempo.** A drifting track gets its average tempo and a grid that wanders off the
  beats. Its confidence is lower, but nothing flags it yet.
- **4/4, with the 1 on the first beat with music.** This is wrong for a track that starts
  on a pickup or a half bar, or whose intro accents a different beat.
- **Tempo traps.** Without a tag, drum & bass (and probably other fast or half-time styles)
  can lock onto two-thirds or half of its tempo. Confidence usually drops when it does,
  which is why the app wants at least 1.0.
- **Keys are tuned for electronic music.** There's no modulation, one key per track, and
  the bass octave is left out. ClubCraft reports 75% exact and 87% exact-or-relative
  against Mixed In Key on 137 tracks, mostly house.
- **Tested only on synthetic audio here.** The unit tests use the demo generator (steady
  tempos, a known 1) and generated chords. Real music is what the test plan is for.

## How it's evaluated

`gui/examples/quick_eval.rs` reads earshot's `library.db` read-only and decodes each track
with the app's own decoder, so times line up with the stored grids. It runs the quick
analysis with and without the tag hint, and compares it with every grid the track has, by
source:

| Source | What it is |
|---|---|
| `analysis` | Beat This! (static) |
| `analysis-dynamic` | Beat This! over the whole track |
| `import` | Another DJ program's, e.g. rekordbox's, when you've imported its library |
| `user` | Fixed by hand: the best reference there is |

For each source it reports:
- **Tempo:** exact (±0.05 BPM), close (±0.5), or half, double, 2/3, 3/2 or wrong; with and
  without the hint.
- **Beat placement:** for tracks whose tempo is right, the median and 90th-percentile
  distance from each quick beat to the nearest reference beat, over the middle 80% of the
  track, and the typical signed offset.
- **Downbeats:** whether the quick 1s land on the reference's 1s.
- **Confidence:** how many tracks the app would use the quick grid for (confidence ≥ 1.0),
  and how many of those are exact.

It also compares keys with the library's key field (exact, relative, a fifth away, or
other) and reports timing: decode and analysis, average and worst. `--csv` writes one row
per track and reference.

### What would decide the next step

These are rough targets, to be revised once real numbers are in:

- **Keep it on load:** at least 90% exact tempo against `analysis` and `user` grids among
  tracks it's confident about, and beats within 20 ms on at least 90% of those. Otherwise
  raise the confidence threshold or go back to the tag placeholder.
- **Make it the static first pass:** at least 95% exact and within 10 ms against
  hand-checked (`user`) grids, with the typical offset fixed. Then Beat This! only has to
  confirm the downbeat and flag drift, which would make static analysis about 10× faster.
- **Store the key:** at least 80% exact-or-relative against tagged keys, which are often
  from Mixed In Key or rekordbox. That needs `track_meta` to accept an `analysis` source
  ranked below `tag`, which is a schema migration.
