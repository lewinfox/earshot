# Beat grids: a fast, accurate straight grid, and a flag for the rest

**Recommendation:** keep today's static analysis, with three changes (`hybrid.py`).
1. Choose its excerpts only from sections with a steady kick.
2. Fall back to the excerpts' own tempo instead of failing.
3. Snap the grid to where the kicks start.

On 63 regular tracks it gets 62 tempos right and puts 57 grids within 10 ms of the kick start. Today's static grids get 2 of 59 within 10 ms. It takes 2.9 s per track. Its flag catches 11 of 12 irregular tracks with 3 false flags.

## Why today's static grids are off

Beat This! (small and full) puts beats 20–40 ms *after* the start of the kick, on every track checked. Static analysis takes its phase from Beat This! small, so its grids are late by about 30 ms (median), steadily. The tempo is fine. The user's rule: a grid line belongs where the kick's attack begins, before the low wave. On a hit that opens with a click, it goes on the click. It never goes on a faint sound before the click.

## Results

75 tracks: 5 named in the brief, 9 with "live"/"rework" style names, 61 at random. Beat This! full is the reference for tempo, for which hits are beats, and for regular/irregular. The phase reference is the start of the hit at each of its beats (`attack.py`), with a straight line through those for regular tracks. Only beats in kick sections count (see below).

63 regular, 12 irregular. Four of the 63 are labelled by hand, following the user's rule that Beat This! is unreliable without a kick:
- 1035 and 1018: the user's verdicts.
- 1659 and 1687: mine (straight apart from an intro or a breakdown).

Phase error is per beat on regular tracks: the grid line minus the start of the kick. "Tempo right" means within 0.05 BPM; half or double tempo counts as right, as the user allowed.

| Approach | Tempo right | Tracks with median phase error ≤ 5 / ≤ 10 ms | Beats within 10 / 20 ms | Irregular flagged | False flags | Time per track |
|---|---|---|---|---|---|---|
| Static (today) | 56/63 (4 fail) | 1 / 2 of 59 | 5% / 20% | 11/12 | 11 | 2.5 s |
| B: static + snap | 56/63 | 37 / 50 | 85% / 94% | 11/12 | 15 | ~2.9 s |
| **Hybrid (recommended)** | **62/63** | **43 / 57** | **89% / 97%** | **11/12** | **3** | **2.9 s** |
| C: Beat This! small, whole track + line + snap | 62/63 | 45 / 59 | 93% / 98% | 11/12 | 5 | 6.8 s |
| D: Beat This! full, whole track + line + snap | 63/63 | 43 / 61 | 94% / 98% | 12/12 | 3 | 11.3 s |
| A: Rust signal processing only | 55/63 | 15 / 29 | 43% / 61% | 11/12 | 12 | 0.5 s |

Notes on the table:
- **Timing setup.** One track at a time on 4 cores (`taskset -c 0-3`, 4 torch threads), normal priority. Models are loaded first, as in the worker. Times are the median of 6 tracks about 4.6 min long and exclude decoding (`timing.py`). B's time is static plus the snap (~0.4 s).
- **D's flag is partly circular**, because it uses the same model run as the labels.
- **A is an early version.** The Rust port has the old phase step, before the hit-start snap. A's real problems are elsewhere: it locks onto off-beat basslines, and it lands mid-kick on kicks that swell.
- **The snap and the reference use the same "start of the hit" rule.** So the phase scores partly agree by construction. The rule was checked by eye on about 10 tracks and agreed with by the user.

Where the hybrid misses 10 ms on a regular track:
- **199, 881, 1239:** 11–12 ms.
- **1132:** a live band, 16 ms; flagged.
- **1527:** drum & bass, tempo 1 BPM off; flagged.
- **1018:** about 15 ms early on the kick by eye (the reference finds few attacks there). D and Beat This! are 50–70 ms late on it.

## The recommended method (`hybrid.py`)

1. **Kick sections.** Kick-band onset strength is summed over 2 s. A kick section is where that sum is above 35% of its 90th percentile for at least 8 s. Percussion-less intros, bass-less breakdowns and fade-outs fall outside.
2. **Excerpts.** The start and end excerpts come from the first and last kick sections, not the first and last sound. The three middle excerpts must lie inside kick sections. If under 30% of the track is kick sections (breakbeats, drum & bass), use the old choice.
3. **Tempo fallback.** If fewer than half the excerpts line up with the first tempo guess, try another tempo. The guess can lock onto 2/3 of a drum & bass tempo, which made static fail on all such tracks. The new tempo comes from the excerpts' own beats (a line through each), refined by the whole-track scan of ±3%. Half and double are tried too. No track in the sample fails now.
4. **Snap.** At every grid line inside a kick section, find the start of the hit (`attack.attacks`):
   - The body of the hit is its loudest point. The gap before it is the quietest point in the 150 ms before that.
   - The hit proper starts where the amplitude first reaches 30% of the way from the gap to the body.
   - Then walk back down that rise to its foot.
   - A faint sound before the click is flat, so it stops the walk-back.

   Move the grid by the median offset. Do it twice, the second time in a ±25 ms window.
5. **Flag** (`hybrid.flag`), from what static already measures on its kick-section excerpts. Flag the track when any of these holds:
   - the excerpts' phases spread by more than 60 ms;
   - two or more excerpts don't line up with the track's tempo at all;
   - one excerpt's own tempo is more than 5% off (half and double allowed).

   The thresholds were tuned on this sample, so expect a few more misses on new tracks. The missed irregular track is 937 (spread 50 ms). The false flags are 1132 (live), 1527 (drum & bass) and 439.

## Things that didn't work

- **A per-beat hit-start flag.** For each grid line, check whether the hit starts within 25 ms. Too noisy: the wide search catches other hits.
- **A window-fold flag** (`flagtest.py`). Fold the kick-band envelope in 8 s windows and compare each window's phase to the whole track's. It put regular tracks hundreds of ms off.
- **Beat This! as phase truth.** It is late, as above. In breakdowns without a kick it also miscounts beats, which bends a straight-line fit. `gtlib.kick_line` numbers each run of kick beats separately to avoid that.

## Files

- `decode_sample.sh`, `rust/src/bin/decode.rs`: decode with the app's own decoder (symphonia via `engine::decode::decode_file`), into the same mono WAV the app sends the worker.
- `sample.py`: choose the sample.
- `bt_run.py`, `run_bt.sh`: Beat This! over whole tracks.
- `attack.py`: start of the hit; kick test.
- `gtlib.py`: ground truth and the scoring metric.
- `hybrid.py`: the recommended method.
- `candidates.py`: run every approach.
- `score.py`: the table.
- `timing.py`: per-track timings.
- `fastgrid.py`, `rust/src/lib.rs`: approach A.
- `plots.py`, `make_review.py`: images and the review page (run with `./pp`, a separate venv with matplotlib).
- `run_all.sh`, `rescore.sh`: rerun everything.
- `py`: the worker's venv, `-I`, cores 0–3.

Data (WAVs, model outputs, grids, images, the review page) lives outside the repo, in `$BG_DATA` (a scratch directory). The library DB was read from a copy; music files were only read. No new models were downloaded.
