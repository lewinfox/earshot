# Commands

Everything earshot can be told to do, by a key (`keymap.txt`), a controller or its own buttons. Made from `gui/src/core/bus.rs` by a test: don't edit by hand (`EARSHOT_WRITE_DOCS=1 cargo test commands_doc` rewrites it).

A name goes from the general to the particular: where, what, which part, what to do. A number in the name says which (`deck_2`, `hot_cue_3`); a number after it is the value (`deck_1_beat_jump -4`). `size` stands for the deck's configured beat jump or loop size.

## Decks: `deck_N_`, N = 1 to 4; hot cue pads M = 1 to 8

| Command | Number | What it does |
|---|---|---|
| `deck_N_play` | — | play / pause |
| `deck_N_cue` | pressed (1) or let go (0) | cue |
| `deck_N_cue_set` | pressed (1) or let go (0) | set the cue point here |
| `deck_N_beat_sync` | — | beat sync |
| `deck_N_quantize` | — | quantise |
| `deck_N_key_lock` | — | key lock |
| `deck_N_headphone_cue` | — | headphone cue |
| `deck_N_hot_cue_M` | pressed (1) or let go (0) | hot cue |
| `deck_N_hot_cue_M_clear` | pressed (1) or let go (0) | clear hot cue |
| `deck_N_beat_jump` | beats (negative: back), or `size` / `-size` | beat jump |
| `deck_N_beat_loop` | beats, or `size` | loop |
| `deck_N_loop_in` | pressed (1) or let go (0) | loop in |
| `deck_N_loop_out` | pressed (1) or let go (0) | loop out |
| `deck_N_loop_exit` | pressed (1) or let go (0) | leave the loop / reloop |
| `deck_N_loop_halve` | pressed (1) or let go (0) | halve the loop |
| `deck_N_loop_double` | pressed (1) or let go (0) | double the loop |
| `deck_N_pitch_bend_down` | pressed (1) or let go (0) | slow down while held |
| `deck_N_pitch_bend_up` | pressed (1) or let go (0) | speed up while held |
| `deck_N_tempo_range_next` | pressed (1) or let go (0) | next tempo range |
| `deck_N_tempo` | -1 to 1 | tempo fader |
| `deck_N_position` | 0 to 1 | go to |
| `deck_N_channel_fader` | 0 to 1 | channel fader |
| `deck_N_trim` | 0 to 4 | trim (gain) |
| `deck_N_eq_low` | 0 to 4 | EQ |
| `deck_N_eq_mid` | 0 to 4 | EQ |
| `deck_N_eq_high` | 0 to 4 | EQ |
| `deck_N_filter` | -1 to 1 | filter |
| `deck_N_memory_cue_save` | pressed (1) or let go (0) | save the cue (or loop) to memory |
| `deck_N_grid_nudge` | milliseconds (negative: earlier) | nudge the grid |
| `deck_N_vinyl_mode` | — | vinyl mode (jog wheels scratch) |
| `deck_N_scratch_hold` | pressed (1) or let go (0) | hand on the record (scratch) |
| `deck_N_scratch` | seconds of track (negative: back) | scratch |
| `deck_N_pitch_bend` | seconds of track (negative: back) | bend the speed |
| `deck_N_load_selected` | pressed (1) or let go (0) | load the selected track |

## Mixer: `mixer_`

| Command | Number | What it does |
|---|---|---|
| `mixer_crossfader` | -1 to 1 | crossfader |
| `mixer_main_level` | 0 to 5 | main level |
| `mixer_main_balance` | -1 to 1 | balance |
| `mixer_headphone_level` | 0 to 5 | headphone level |
| `mixer_headphone_mix` | 0 to 1 | headphone mix |

## Effect units: `fx_N_`, N = 1 to 1

| Command | Number | What it does |
|---|---|---|
| `fx_N_on` | — | effect on / off |
| `fx_N_level` | 0 to 1 | effect level |
| `fx_N_beats_halve` | pressed (1) or let go (0) | halve the effect's length |
| `fx_N_beats_double` | pressed (1) or let go (0) | double the effect's length |
| `fx_N_effect_next` | pressed (1) or let go (0) | next effect |
| `fx_N_channel_next` | pressed (1) or let go (0) | effect on the next deck / main |

## Library: `library_`

| Command | Number | What it does |
|---|---|---|
| `library_browse` | rows (negative: up) | move through the list |
| `library_pane_toggle` | pressed (1) or let go (0) | library / suggestions |
| `library_search` | pressed (1) or let go (0) | search |
| `library_undo` | pressed (1) or let go (0) | undo |
| `library_redo` | pressed (1) or let go (0) | redo |
