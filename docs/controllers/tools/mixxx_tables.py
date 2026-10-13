#!/usr/bin/env python3
"""Turn usage.json (from mixxx_usage.py) into the tables in mixxx-controls.md.

  python3 -I mixxx_tables.py usage.json ../mixxx-controls.md

Rewrites the part of the doc between the GENERATED markers and prints headline
numbers. The earshot status of each Mixxx control is the STATUS table below: keep
it up to date as earshot gains controls (gui/src/core/bus.rs, Control::from_mixxx).
"""

import collections
import json
import re
import sys

# ---- earshot's status for each Mixxx control ----------------------------------------
#
# One line each:  [family:]name | status | earshot equivalent | needs
#
# status   have       works today through Control::from_mixxx
#          name       earshot can already do it (a command or snapshot field exists);
#                     only the Mixxx name needs wiring up, maybe with a unit change
#          partial    works for some groups or with different meaning (see note)
#          missing    needs new work in earshot (see needs)
#          n/a        Mixxx's own GUI or features earshot won't have
# needs    what the missing work is:
#          small      a small engine or app feature (a command, a seek, a flag)
#          ui         library / window navigation in the app
#          led        an output computed in the app from the snapshot (blinks, beats)
#          scratch    jog wheels and scratching
#          slip       slip mode, reverse, rolls, censor
#          key        key shift / pitch / key sync
#          fx         effect units
#          sampler    sampler decks
#          stems      stem separation
#          inputs     microphone / auxiliary inputs
#          vinyl      timecode vinyl control
#          autodj     Auto DJ
#          markers    intro / outro markers
#          rec        recording the mix
#
# A family prefix (deck:, mixer:, mic_aux:, effects:, library:, …) picks one row when
# a name is used in several groups.

STATUS = """
deck:volume | have | Volume |
deck:pregain | have | Trim |
deck:play | have | Play |
deck:cue_default | have | Cue (CDJ style) |
deck:cue_set | have | SetCue |
deck:sync_enabled | have | Sync |
deck:quantize | have | Quantize |
deck:keylock | have | Keylock |
deck:pfl | have | Pfl |
hotcue_N_activate | have | Hotcue(n) |
hotcue_N_clear | have | HotcueClear(n) |
beatjump | have | BeatJump |
beatjump_backward | have | JumpBack |
beatjump_forward | have | JumpForward |
beatjump_size | have | JumpSize |
beatloop_activate | have | Loop |
beatloop_size | have | LoopSize |
beatloop_N_toggle | have | BeatLoop(n) |
loop_in | have | LoopIn |
loop_out | have | LoopOut |
reloop_toggle | have | Reloop |
loop_halve | have | LoopHalve |
loop_double | have | LoopDouble |
rate_temp_down | have | NudgeDown | note: earshot's nudge is a fixed 4 %
rate_temp_up | have | NudgeUp |
rate | have | Tempo (TempoFader) |
playposition | have | Position |
LoadSelectedTrack | have | LoadSelected |
rateRange | partial | TempoRangeNext | earshot treats it as a "next range" button; Mixxx's is a value (0.08 = ±8 %). Needs mapping to the nearest TempoRange
deck:mute | missing | | small
deck:vu_meter | name | DeckSnap.vu |
deck:vu_meter_left | partial | DeckSnap.vu (mono) | small
deck:vu_meter_right | partial | DeckSnap.vu (mono) | small
deck:peak_indicator | name | DeckSnap.vu ≥ 1 |
deck:peak_indicator_left | partial | DeckSnap.vu (mono) | small
deck:peak_indicator_right | partial | DeckSnap.vu (mono) | small
show_track_menu | missing | | ui
back | missing | (Seek exists) | small
fwd | missing | (Seek exists) | small
rateSearch | missing | (Seek exists) | small
beat_closest | missing | (grid in DeckSession) | led
beat_distance | missing | (grid in DeckSession) | led
beat_active | missing | (grid in DeckSession) | led
beatjump_size_double | name | SetJumpSize |
beatjump_size_halve | name | SetJumpSize |
beatjump_N_backward | name | BeatJump(-n) |
beatjump_N_forward | name | BeatJump(n) |
beatloop_rN_activate | missing | | small
beatloop_N_activate | name | BeatLoop(n) | note: activate always (re)starts; earshot's toggles
beatloop_N_enabled | name | DeckSnap loop_active + length |
beatlooproll_activate | missing | | slip
beatlooproll_rN_activate | missing | | slip
beatlooproll_N_activate | missing | | slip
beats_adjust_faster | name | GridOp::SetBpm |
beats_adjust_slower | name | GridOp::SetBpm |
beats_set_double | name | GridOp::Double |
beats_set_halve | name | GridOp::Halve |
beats_set_fourthirds | name | GridOp::SetBpm |
beats_set_threefourths | name | GridOp::SetBpm |
beats_set_threehalves | name | GridOp::SetBpm |
beats_set_twothirds | name | GridOp::SetBpm |
beats_translate_curpos | name | GridOp::AnchorHere |
beats_translate_earlier | name | GridOp::Nudge (grid_nudge) |
beats_translate_later | name | GridOp::Nudge (grid_nudge) |
beats_translate_half | missing | | small
beats_translate_match_alignment | missing | | small
beats_undo_adjustment | name | Undo |
beatsync | missing | (Sync exists) | small
beatsync_phase | missing | | small
beatsync_tempo | missing | | small
bpm | name | DeckSnap.bpm (read); setting it sets the tempo |
bpm_tap | missing | | small
tempo_tap | missing | | small
bpmlock | missing | | small
CloneFromDeck | missing | | small
CloneFromSampler | missing | | sampler
cue_cdj | name | Cue |
cue_clear | missing | | small
cue_goto | name | Seek to DeckSnap.cue_point |
cue_gotoandplay | name | Seek + Play |
cue_gotoandstop | name | Seek + Play(Off) |
cue_indicator | missing | (DeckSnap.playing, cue_point) | led
cue_mode | n/a | earshot has one cue mode (CDJ) |
cue_play | missing | | small
cue_point | name | DeckSnap.cue_point / SetCue(Some) | note: Mixxx counts samples, earshot seconds
cue_preview | missing | | small
cue_simple | missing | | small
duration | name | DeckSnap.duration |
eject | missing | | small
end | name | Seek |
start | name | Seek |
start_play | name | Seek + Play |
start_stop | name | Seek + Play(Off) |
stop | name | Play(Off) |
hotcue_focus | missing | | small
hotcue_focus_color_next | missing | | small
hotcue_focus_color_prev | missing | | small
hotcue_N_activatecue | name | Hotcue(n) |
hotcue_N_activateloop | missing | | small
hotcue_N_setloop | missing | | small
hotcue_N_cueloop | missing | | small
hotcue_N_gotoandloop | missing | | small
hotcue_N_color | partial | memory cues have colours; hot cues only labels | small
hotcue_N_goto | name | Seek to hot cue |
hotcue_N_gotoandplay | name | Seek + Play |
hotcue_N_gotoandstop | name | Seek + Play(Off) |
hotcue_N_position | name | DeckSnap.hotcues[n] | note: samples vs seconds
hotcue_N_set | missing | (Hotcue sets an empty pad) | small
hotcue_N_setcue | missing | (Hotcue sets an empty pad) | small
hotcue_N_status | name | DeckControl::Hotcue get |
hotcue_N_enabled | name | DeckControl::Hotcue get | alias of hotcue_N_status
hotcue_N_swap | missing | | small
hotcue_N_type | name | always 1 (cue) |
intro_end_activate | missing | | markers
intro_end_clear | missing | | markers
intro_end_position | missing | | markers
intro_end_set | missing | | markers
intro_start_activate | missing | | markers
intro_start_clear | missing | | markers
intro_start_position | missing | | markers
intro_start_set | missing | | markers
outro_end_activate | missing | | markers
outro_end_clear | missing | | markers
outro_end_position | missing | | markers
outro_end_set | missing | | markers
outro_start_activate | missing | | markers
outro_start_clear | missing | | markers
outro_start_position | missing | | markers
outro_start_set | missing | | markers
intro_end_enabled | missing | | markers
intro_start_enabled | missing | | markers
outro_end_enabled | missing | | markers
outro_start_enabled | missing | | markers
jog | missing | | scratch
scratch2 | missing | | scratch
scratch2_enable | missing | | scratch
scratch_position | missing | | scratch
scratch_position_enable | missing | | scratch
wheel | missing | | scratch
key | missing | (key is analysed; no key shift) | key
pitch | missing | | key
pitch_adjust | missing | | key
pitch_down | missing | | key
pitch_up | missing | | key
reset_key | missing | | key
sync_key | missing | | key
visual_key | name | the track's effective key (UI) |
visual_key_distance | missing | | key
file_key | name | the track's key |
LoadSelectedTrackAndPlay | name | LoadSelected + Play |
LoadTrackFromDeck | missing | | small
LoadTrackFromPreviewDeck | missing | | small
LoadTrackFromSampler | missing | | sampler
local_bpm | name | DeckSnap.bpm |
visual_bpm | name | DeckSnap.bpm |
file_bpm | name | the track's BPM |
loop_anchor | missing | | small
loop_enabled | name | DeckSnap.loop_active |
loop_start_position | name | DeckSnap.loop_in / SetLoop | note: samples vs seconds
loop_end_position | name | DeckSnap.loop_out / SetLoop | note: samples vs seconds
loop_in_goto | missing | | small
loop_out_goto | missing | | small
loop_move | missing | | small
loop_move_N_backward | missing | | small
loop_move_N_forward | missing | | small
loop_remove | missing | | small
loop_scale | name | engine LoopScale |
deck:orientation | name | DeckCmd::Crossfader(XfSide) |
passthrough | missing | | vinyl
play_indicator | missing | (DeckSnap.playing) | led
play_latched | name | DeckSnap.playing |
play_stutter | missing | | small
rate_dir | name | a constant (earshot's fader direction is fixed) |
rate_perm_down | name | engine Rate |
rate_perm_down_small | name | engine Rate |
rate_perm_up | name | engine Rate |
rate_perm_up_small | name | engine Rate |
rate_temp_down_small | name | Nudge |
rate_temp_up_small | name | Nudge |
rateEngine | name | DeckSnap.speed |
reloop_andstop | missing | | small
reloop_exit | name | Reloop | alias of reloop_toggle
repeat | missing | | small
reverse | missing | | slip
reverseroll | missing | | slip
slip_enabled | missing | | slip
shift_cues_earlier | missing | | small
shift_cues_earlier_small | missing | | small
shift_cues_later | missing | | small
shift_cues_later_small | missing | | small
sort_hotcues | missing | | small
sort_hotcues_remove_offsets | missing | | small
deck:stars_down | name | Edit Rating |
deck:stars_up | name | Edit Rating |
stars_one | name | Edit Rating |
stars_two | name | Edit Rating |
stars_three | name | Edit Rating |
stars_four | name | Edit Rating |
stars_five | name | Edit Rating |
sync_leader | name | SetSyncLeader / MixerSnap.sync_leader |
sync_master | name | SetSyncLeader | alias of sync_leader
sync_mode | name | DeckSnap.sync / following |
track_color | name | Edit Colour |
update_replaygain_from_pregain | missing | | small
vinylcontrol_cueing | missing | | vinyl
vinylcontrol_enabled | missing | | vinyl
vinylcontrol_mode | missing | | vinyl
vinylcontrol_status | missing | | vinyl
waveform_zoom | name | ViewCmd::WaveZoom |
waveform_zoom_down | name | ViewCmd::WaveZoom |
waveform_zoom_set_default | name | ViewCmd::WaveZoom |
waveform_zoom_up | name | ViewCmd::WaveZoom |
stem_count | missing | | stems
end_of_track | name | EngineEvent::TrackEnded / position |
track_loaded | name | DeckSnap.track_id |
track_samplerate | name | the track's sample rate |
track_samples | name | DeckSnap.duration × rate |
beatloop | name | BeatLoop | deprecated
beatloop_N | name | BeatLoop(n) | deprecated
filter | missing | (filter has no on/off) | small
filterDepth | name | Filter | deprecated
filterHigh | name | Eq(EQ_HIGH) | alias of the EQ rack's parameter3
filterMid | name | Eq(EQ_MID) | alias of the EQ rack's parameter2
filterLow | name | Eq(EQ_LOW) | alias of the EQ rack's parameter1
filterHighKill | missing | | small
filterMidKill | missing | | small
filterLowKill | missing | | small
deck:PeakIndicator | name | DeckSnap.vu | deprecated
deck:PeakIndicatorN | partial | DeckSnap.vu (mono) | small
deck:VuMeter | name | DeckSnap.vu | deprecated
deck:VuMeterN | partial | DeckSnap.vu (mono) | small

mixer:crossfader | have | Master(Crossfader) |
mixer:gain | have | Master(Gain) |
mixer:balance | have | Master(Balance) |
mixer:headGain | have | Master(HeadGain) |
mixer:headMix | have | Master(HeadMix) |
mixer:headVolume | name | Master(HeadGain) | deprecated alias
mixer:volume | name | Master(Gain) | deprecated alias
crossfader_down | name | Master(Crossfader) |
crossfader_up | name | Master(Crossfader) |
crossfader_down_small | name | Master(Crossfader) |
crossfader_up_small | name | Master(Crossfader) |
mixer:vu_meter | name | MixerSnap.vu_main |
mixer:vu_meter_left | name | MixerSnap.vu_main[0] |
mixer:vu_meter_right | name | MixerSnap.vu_main[1] |
mixer:peak_indicator | name | MixerSnap.vu_main / limiting |
mixer:peak_indicator_left | name | MixerSnap.vu_main[0] |
mixer:peak_indicator_right | name | MixerSnap.vu_main[1] |
mixer:VuMeter | name | MixerSnap.vu_main | deprecated
mixer:VuMeterN | name | MixerSnap.vu_main | deprecated
mixer:PeakIndicator | name | MixerSnap.vu_main | deprecated
mixer:PeakIndicatorN | name | MixerSnap.vu_main | deprecated
booth_enabled | missing | | small
booth_gain | missing | | small
duckStrength | missing | | inputs
talkoverDucking | missing | | inputs
mixer:enabled | name | constant 1 |
headEnabled | name | routing has a headphone bus |
headSplit | missing | | small
latency | name | buffer size / sample rate |
num_effectsavailable | missing | | fx
mixer:audio_latency_overload | name | Stats.xruns | deprecated
mixer:audio_latency_overload_count | name | Stats.xruns | deprecated
mixer:audio_latency_usage | name | Stats.max_callback_ns | deprecated
guiTick50ms | name | a UI clock | deprecated
guiTickTime | name | a UI clock | deprecated
maximize_library | n/a | | deprecated skin control
mixer:num_auxiliaries | name | 0 | deprecated
mixer:num_decks | name | EngineSnapshot.decks_live | deprecated
mixer:num_microphones | name | 0 | deprecated
mixer:num_preview_decks | name | 1 | deprecated
mixer:num_samplers | missing | | sampler
mixer:samplerate | name | EngineSnapshot.sample_rate | deprecated

app:audio_latency_overload | name | Stats.xruns |
app:audio_latency_overload_count | name | Stats.xruns |
app:audio_latency_usage | name | Stats.max_callback_ns |
app:num_auxiliaries | name | 0 |
app:num_decks | name | EngineSnapshot.decks_live |
app:num_microphones | name | 0 |
app:num_preview_decks | name | 1 (Prepare's player) |
app:num_samplers | missing | | sampler
app:samplerate | name | EngineSnapshot.sample_rate |
gui_tick_50ms_period_s | name | a UI clock |
gui_tick_full_period_s | name | a UI clock |
indicator_250ms | name | a UI clock |
indicator_500ms | name | a UI clock |

library:MoveVertical | have | Browse |
library:MoveUp | name | Browse(-1) |
library:MoveDown | name | Browse(1) |
library:ScrollVertical | name | Browse (by a page) |
library:ScrollUp | name | Browse (by a page) |
library:ScrollDown | name | Browse (by a page) |
library:MoveHorizontal | missing | | ui
library:MoveLeft | missing | | ui
library:MoveRight | missing | | ui
library:MoveFocus | partial | TogglePane / FocusPane | earshot's panes differ from Mixxx's sidebar + table
library:MoveFocusForward | partial | TogglePane / FocusPane |
library:MoveFocusBackward | partial | TogglePane / FocusPane |
library:GoToItem | partial | LoadSelected / open the source | ui
library:clear_search | missing | | ui
library:focused_widget | missing | | ui
library:font_size_decrement | n/a | |
library:font_size_increment | n/a | |
library:font_size_knob | n/a | |
library:search_history_next | missing | | ui
library:search_history_prev | missing | | ui
library:search_history_selector | missing | | ui
library:set_bpmlock | missing | | small
library:show_track_menu | missing | | ui
library:sort_column | missing | | ui
library:sort_column_toggle | missing | | ui
library:sort_focused_column | missing | | ui
library:sort_order | missing | | ui
library:stars_down | name | Edit Rating |
library:stars_up | name | Edit Rating |
library:track_color_next | name | Edit Colour |
library:track_color_prev | name | Edit Colour |
library:AutoDjAddBottom | missing | | autodj
library:AutoDjAddTop | missing | | autodj
library:LoadSelectedIntoFirstStopped | name | LoadSelected (first stopped deck) |
library:SelectNextPlaylist | missing | (Source exists) | ui
library:SelectPrevPlaylist | missing | (Source exists) | ui
library:SelectPlaylist | missing | (Source exists) | ui
library:SelectNextTrack | name | Browse(1) |
library:SelectPrevTrack | name | Browse(-1) |
library:SelectTrackKnob | name | Browse |
library:ToggleSelectedSidebarItem | missing | | ui
library:show_coverart | n/a | | deprecated skin control

effects:super1 | partial | Filter | have for [QuickEffectRack1_[ChannelN]] (the filter); missing for effect units
effects:parameterN | partial | Eq(band) | have for the EQ rack's parameter1-3; missing for effect units
effects:button_parameterN | missing | (EQ kills) | fx
effects:enabled | missing | | fx
effects:group_[ChannelN]_enable | missing | | fx

controls:AutoHotcueColors | n/a | |
controls:ShowDurationRemaining | n/a | |
controls:touch_shift | n/a | |
"""

FAMILY_DEFAULT = {
    "autodj": ("missing", "", "autodj"),
    "mic_aux": ("missing", "", "inputs"),
    "effects": ("missing", "", "fx"),
    "stem": ("missing", "", "stems"),
    "sampler": ("missing", "", "sampler"),
    "samplers": ("n/a", "", ""),
    "recording": ("missing", "", "rec"),
    "shoutcast": ("n/a", "", ""),
    "skin": ("n/a", "", ""),
    "vinylcontrol": ("missing", "", "vinyl"),
    "waveform": ("missing", "", "stems"),
    "controls": ("n/a", "", ""),
}

FAMILY_TITLE = {
    "deck": "Decks: [ChannelN], [PreviewDeckN], [SamplerN] (and stem sub-decks)",
    "mixer": "Mixer: [Master], [Main]",
    "app": "[App]",
    "library": "Library: [Library], [Playlist]",
    "effects": "Effects: [EffectRack1_…], [EqualizerRack1_…], [QuickEffectRack1_…]",
    "mic_aux": "Inputs: [MicrophoneN], [AuxiliaryN]",
    "autodj": "[AutoDJ]",
    "sampler": "[Sampler] (banks)",
    "recording": "[Recording]",
    "vinylcontrol": "[VinylControl]",
    "stem": "Stems: [ChannelN_StemM]",
    "waveform": "[Waveform]",
    "skin": "[Skin]",
    "controls": "[Controls]",
    "shoutcast": "[Shoutcast]",
    "samplers": "[Samplers] (deprecated)",
}
FAMILY_ORDER = list(FAMILY_TITLE)

NEEDS_TITLE = {
    "small": "a small engine or app feature",
    "led": "an LED output worked out in the app",
    "ui": "library or window navigation",
    "scratch": "jog wheels and scratching",
    "slip": "slip, reverse and rolls",
    "key": "key shift and key sync",
    "fx": "effect units",
    "sampler": "sampler decks",
    "stems": "stems",
    "inputs": "microphone and aux inputs",
    "vinyl": "timecode vinyl",
    "autodj": "Auto DJ",
    "markers": "intro / outro markers",
    "rec": "recording",
}


def load_status():
    by_fam, by_name = {}, {}
    for line in STATUS.strip().splitlines():
        if not line.strip():
            continue
        parts = [p.strip() for p in line.split("|")]
        name, status, equiv, needs = (parts + ["", "", ""])[:4]
        note = ""
        if needs and needs not in NEEDS_TITLE:
            note, needs = needs, ""
            if note.startswith("note: "):
                note = note[6:]
        row = (status, equiv, needs, note)
        if ":" in name:
            fam, n = name.split(":", 1)
            by_fam[(fam, n)] = row
        else:
            by_name[name] = row
    return by_fam, by_name


def status_of(c, by_fam, by_name):
    key = (c["family"], c["name"])
    if key in by_fam:
        return by_fam[key]
    if c["name"] in by_name and c["family"] in ("deck", "stem"):
        return by_name[c["name"]]
    if c["name"] in by_name and c["family"] not in FAMILY_DEFAULT:
        return by_name[c["name"]]
    if c["family"] in FAMILY_DEFAULT:
        s, e, n = FAMILY_DEFAULT[c["family"]]
        return (s, e, n, "")
    return ("missing", "", "small", "UNCLASSIFIED")


def kind(c):
    r = c["range"].strip()
    low = r.lower()
    if c["section"] == "ro":
        pre = "read-only "
    else:
        pre = ""
    if low.startswith("binary") or low in ("bool", "toggle"):
        k = "binary"
    elif "|value|" in low:
        k = "enum"
    elif low.startswith("relative") or low in ("+1/-1", "-n / +n", "-1 or 1"):
        k = "relative"
    elif "integer" in low:
        k = "integer"
    elif low in ("", "?", "default", "fixme"):
        k = "value"
    elif "rgb" in low:
        k = "colour"
    else:
        k = r if len(r) <= 22 else "value"
        k = k.replace("|", "/")
    return pre + k


def short(desc, n=90):
    d = re.sub(r"\s+", " ", desc).strip()
    d = re.sub(r"\s*This is a \w+ control\.?", ".", d).replace("..", ".")
    d = d.replace("|", "/")
    m = re.match(r"(.+?[.!?])(\s|$)", d)
    if m and len(m.group(1)) <= n:
        return m.group(1)
    return d if len(d) <= n else d[: n - 1].rsplit(" ", 1)[0] + "…"


def main():
    usage_path, doc_path = sys.argv[1], sys.argv[2]
    u = json.load(open(usage_path))
    by_fam, by_name = load_status()
    nmaps = len(u["mappings"])
    use = u["usage"]

    def used(name, groups=None):
        """(any, xml in, xml out, js) mapping counts. XML counts only the given
        groups (all groups when None); JS can't tell groups apart."""
        d = use.get(name, {"xml_in": {}, "xml_out": {}, "js": []})
        pick = lambda gm: set().union(*[set(m) for g, m in gm.items() if groups is None or g in groups])
        xi, xo, js = pick(d["xml_in"]), pick(d["xml_out"]), set(d["js"])
        return len(xi | xo | js), len(xi), len(xo), len(js)

    def xml_weight(name, groups):
        d = use.get(name, {"xml_in": {}, "xml_out": {}})
        return sum(len(m) for k in ("xml_in", "xml_out") for g, m in d[k].items() if g in groups)

    rows = []
    for c in u["controls"]:
        st, eq, needs, note = status_of(c, by_fam, by_name)
        if c["section"] == "dep" and "deprecated" not in note and "alias" not in note:
            note = (note + "; " if note else "") + "deprecated"
        rows.append(dict(c, status=st, equiv=eq, needs=needs, note=note,
                         use=used(c["name"], set(c["groups"])),
                         weight=xml_weight(c["name"], set(c["groups"]))))
    unclassified = [r["name"] for r in rows if r["note"] == "UNCLASSIFIED"]

    out = []
    w = out.append
    # Summary by family (current controls only: deprecated aliases left out).
    w("### Coverage by group\n")
    w("Current controls only (deprecated aliases are listed in the tables but not counted"
      " here). *Coverage* is **have** ÷ (all − n/a); *with names* adds **name** and"
      " **partial**: what a translation layer gets without new engine work.\n")
    w("| Group | Controls | have | partial | name | missing | n/a | Coverage | With names |")
    w("|---|--:|--:|--:|--:|--:|--:|--:|--:|")
    tot = collections.Counter()
    fams = sorted({r["family"] for r in rows}, key=lambda f: FAMILY_ORDER.index(f) if f in FAMILY_ORDER else 99)
    for f in fams:
        rs = [r for r in rows if r["family"] == f and r["section"] != "dep"]
        if not rs:
            continue
        c = collections.Counter(r["status"] for r in rs)
        n = len(rs)
        live = n - c["n/a"]
        tot.update(c)
        tot["all"] += n
        cov = f"{c['have'] / live:.0%}" if live else "–"
        cov2 = f"{(c['have'] + c['name'] + c['partial']) / live:.0%}" if live else "–"
        w(f"| {FAMILY_TITLE.get(f, f).split(':')[0]} | {n} | {c['have']} | {c['partial']} | {c['name']} | "
          f"{c['missing']} | {c['n/a']} | {cov} | {cov2} |")
    live = tot["all"] - tot["n/a"]
    w(f"| **All** | **{tot['all']}** | **{tot['have']}** | **{tot['partial']}** | **{tot['name']}** | "
      f"**{tot['missing']}** | **{tot['n/a']}** | **{tot['have'] / live:.0%}** | "
      f"**{(tot['have'] + tot['name'] + tot['partial']) / live:.0%}** |")
    w("")
    headline = dict(tot)

    # Usage-weighted: of all (mapping, control) uses, how many hit each status.
    # One row per name (a name in several groups counts once).
    # A name in several groups: the row whose groups the XML uses most, else (JS
    # only) the first of deck, effects, mixer, library, … .
    pref = {f: i for i, f in enumerate(["deck", "effects", "mixer", "app", "library"])}
    seen = {}
    for r in sorted(rows, key=lambda r: (r["section"] == "dep", -r["weight"], pref.get(r["family"], 9))):
        if r["name"] not in seen:
            seen[r["name"]] = dict(r, use=used(r["name"]))
    uses = collections.Counter()
    for r in seen.values():
        uses[r["status"]] += r["use"][0]
    total_uses = sum(uses.values())
    w("### Weighted by what mappings use\n")
    w(f"Counting each (mapping, control) pair once across all {nmaps} mappings: "
      f"{total_uses} uses.\n")
    w("| Status | Uses | Share |")
    w("|---|--:|--:|")
    for s in ("have", "partial", "name", "missing", "n/a"):
        w(f"| {s} | {uses[s]} | {uses[s] / total_uses:.0%} |")
    w("")
    headline["weighted"] = {s: uses[s] / total_uses for s in uses}

    # Top controls.
    top = sorted(seen.values(), key=lambda r: -r["use"][0])[:50]
    w("### The 50 controls mappings use most\n")
    w(f"*Used by* is how many of the {nmaps} mappings use the control at all; *XML in*,"
      " *XML out* and *JS* split that by where it appears (see the method notes).\n")
    w("| # | Control | Group | Used by | XML in | XML out | JS | earshot | Equivalent / needs |")
    w("|--:|---|---|--:|--:|--:|--:|---|---|")
    for i, r in enumerate(top, 1):
        a, xi, xo, js = r["use"]
        eqn = r["equiv"]
        if r["needs"]:
            eqn += ("; " if eqn else "") + f"*{NEEDS_TITLE.get(r['needs'], r['needs'])}*"
        if r["note"]:
            eqn += f" ({r['note']})" if eqn else r["note"]
        gm = collections.Counter()
        for k in ("xml_in", "xml_out"):
            for g, m in use.get(r["name"], {}).get(k, {}).items():
                gm[g] += len(m)
        grp = gm.most_common(1)[0][0] if gm else r["groups"][0]
        w(f"| {i} | `{r['name']}` | {grp} | {a} | {xi} | {xo} | {js} | {r['status']} | {eqn} |")
    w("")

    # Missing by need.
    w("### What the missing controls need\n")
    w("Current controls with status **missing** (and **partial** ones that still need"
      " work), by the work they need. *Uses* sums how many mappings use each one.\n")
    w("| Needs | Controls | Uses | Most used |")
    w("|---|--:|--:|---|")
    need_rows = collections.defaultdict(list)
    for r in seen.values():
        if r["section"] == "dep":
            continue
        if r["status"] in ("missing", "partial") and r["needs"]:
            need_rows[r["needs"]].append(r)
    for nd, rs in sorted(need_rows.items(), key=lambda kv: -sum(r["use"][0] for r in kv[1])):
        rs.sort(key=lambda r: -r["use"][0])
        most = ", ".join(f"`{r['name']}` ({r['use'][0]})" for r in rs[:5])
        w(f"| {NEEDS_TITLE[nd]} | {len(rs)} | {sum(r['use'][0] for r in rs)} | {most} |")
    w("")

    # Per-mapping coverage at each stage.
    stage_ok = {
        "today": {"have"},
        "names": {"have", "name", "partial"},
    }
    small = {"small", "led", "ui"}
    status_by_name = {n: r for n, r in seen.items()}
    per_map = collections.defaultdict(set)
    for n, d in use.items():
        for k in ("xml_in", "xml_out", "js"):
            for m in d[k]:
                per_map[m].add(n)

    def ok(r, stage):
        if r["status"] == "n/a":
            return True
        if r["status"] in stage_ok.get(stage, {"have", "name", "partial"}):
            return True
        if stage in ("small", "scratch", "all") and r["needs"] in small:
            return True
        if stage in ("scratch", "all") and r["needs"] in ("scratch", "slip"):
            return True
        if stage == "all":
            return True
        return False

    w("### How many mappings would be fully covered\n")
    w("For each mapping, the share of the controls it uses that earshot would have at"
      " each stage (n/a controls count as covered: a mapping touching them just gets a"
      " no-op). This is about control names only; running the mapping's JS is a"
      " separate requirement (see mixxx-mappings.md).\n")
    w("| Stage | Mappings 100 % covered | ≥ 90 % | ≥ 75 % | Median coverage |")
    w("|---|--:|--:|--:|--:|")
    stages = [("today", "Today (Control::from_mixxx)"),
              ("names", "+ names and aliases for what earshot already does"),
              ("small", "+ small features, LED outputs, library navigation"),
              ("scratch", "+ jog / scratch and slip / reverse")]
    headline["stages"] = {}
    for st, title in stages:
        covs = []
        for m in sorted(per_map):
            ns = [status_by_name[n] for n in per_map[m] if n in status_by_name]
            if not ns:
                continue
            covs.append(sum(ok(r, st) for r in ns) / len(ns))
        covs.sort()
        med = covs[len(covs) // 2]
        full = sum(c >= 0.999 for c in covs)
        n90 = sum(c >= 0.9 for c in covs)
        n75 = sum(c >= 0.75 for c in covs)
        w(f"| {title} | {full} ({full / len(covs):.0%}) | {n90} ({n90 / len(covs):.0%}) | "
          f"{n75} ({n75 / len(covs):.0%}) | {med:.0%} |")
        headline["stages"][st] = (full, n90, n75, med, len(covs))
    w("")

    # Full tables.
    w("## Every control, by group\n")
    w("*Type* comes from the manual's range: binary (a button or switch), a numeric"
      " range (a knob or fader), relative (an encoder), enum, or read-only (an output"
      " for LEDs and scripts). Names with `N` stand for a family (`hotcue_N_activate` is"
      " `hotcue_1_activate` … `hotcue_36_activate`). Pot-meter controls also take the"
      " `_up`, `_down`, `_set_default`, `_set_zero`, `_toggle` … suffixes (marked ±)."
      " *Used by* counts XML uses in this table's groups, but JS uses by name only, so"
      " a name that also exists in another group (marked †) may count the other"
      " group's JS uses too.\n")
    fam_of = collections.defaultdict(set)
    for r in rows:
        fam_of[r["name"]].add(r["family"])
    shared = {n for n, fs in fam_of.items() if len(fs) > 1}
    for f in fams:
        rs = [r for r in rows if r["family"] == f]
        w(f"### {FAMILY_TITLE.get(f, f)}\n")
        groups = sorted({g for r in rs for g in r["groups"]})
        w("Groups: " + ", ".join(f"`{g}`" for g in groups) + ".\n")
        w("| Control | What it does | Type | earshot | Equivalent / needs | Used by (XML in / out / JS) |")
        w("|---|---|---|---|---|---|")
        order = {"have": 0, "partial": 1, "name": 2, "missing": 3, "n/a": 4}
        for r in sorted(rs, key=lambda r: (r["section"] == "dep", order[r["status"]], -r["use"][0], r["name"])):
            a, xi, xo, js = r["use"]
            nm = f"`{r['name']}`" + (" ±" if r["pot_suffixes"] else "") + (" †" if r["name"] in shared else "")
            eqn = r["equiv"]
            if r["needs"]:
                eqn += ("; " if eqn else "") + f"*{NEEDS_TITLE[r['needs']]}*"
            if r["note"]:
                eqn += ("; " if eqn else "") + r["note"]
            if len(r["groups"]) > 1 and f != "deck":
                eqn += ("; " if eqn else "") + "in " + ", ".join(r["groups"])
            st = "**have**" if r["status"] == "have" else r["status"]
            usage = f"{a} ({xi} / {xo} / {js})" if a else "–"
            w(f"| {nm} | {short(r['desc'])} | {kind(r)} | {st} | {eqn} | {usage} |")
        w("")

    gen = "\n".join(out)
    doc = open(doc_path).read()
    a, b = "<!-- BEGIN GENERATED -->", "<!-- END GENERATED -->"
    i, j = doc.index(a) + len(a), doc.index(b)
    doc = doc[:i] + "\n" + gen + "\n" + doc[j:]
    open(doc_path, "w").write(doc)

    print("unclassified:", unclassified)
    print(json.dumps(headline, indent=1, default=str))


if __name__ == "__main__":
    main()
