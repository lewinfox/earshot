"""static_tempo: the tempo from excerpts across a track, which excerpts, and the flag."""

import numpy as np

from earshot_worker.static_tempo import (
    EXCERPT_S,
    EXCERPTS,
    HOP,
    SR,
    STEADY_MS,
    choose_tempo,
    excerpt_starts,
    grid_fit,
    is_steady,
)
from earshot_worker.snap import kick_sections

# Five excerpts, start to end of a five-minute track, as analyse_static picks them.
CENTRES = np.array([8.0, 70.0, 150.0, 230.0, 292.0])
W = np.ones(5)


def anchors(bpm: float, jitter_ms: float = 0.0, seed: int = 1) -> np.ndarray:
    """A beat near each excerpt's centre, on a straight grid at `bpm` (+ jitter)."""
    p = 60 / bpm
    a = np.round(CENTRES / p) * p + 0.11
    return a + np.random.default_rng(seed).uniform(-1, 1, len(a)) * jitter_ms / 1000


def test_whole_number_kept_despite_detection_jitter():
    assert choose_tempo(anchors(128.0, jitter_ms=4), W, raw=128.03) == 128.0


def test_slightly_off_raw_tempo_is_corrected():
    # The whole-track pass said 123.93 (it's good to a few hundredths; much further
    # off and the excerpts' beats can't be numbered across the track). The excerpts'
    # phases creep, giving 123.9.
    bpm = choose_tempo(anchors(123.9), W, raw=123.93)
    assert bpm == 123.9
    _, miss = grid_fit(anchors(123.9), W, 60 / bpm)
    assert np.abs(miss).max() < 0.015


def test_nothing_finer_than_a_tenth():
    # A track at 120.76 (an edit, say) gets the nearest tenth; the grid then misses
    # the ends, which flags it as drifting so the app suggests dynamic analysis.
    a = anchors(120.76)
    bpm = choose_tempo(a, W, raw=120.79)
    assert bpm == 120.8
    _, miss = grid_fit(a, W, 60 / bpm)
    assert np.ptp(miss) * 1000 > STEADY_MS


def test_irregular_track_shows_as_spread():
    # A tempo that wanders (live drummer): no straight grid fits, so the excerpts
    # stay spread out after fitting, and analyse_static flags it as drifting.
    a = anchors(124.0) + np.array([0, 0.03, -0.035, 0.04, 0])
    bpm = choose_tempo(a, W, raw=124.0)
    _, miss = grid_fit(a, W, 60 / bpm)
    assert np.ptp(miss) * 1000 > STEADY_MS


def test_few_excerpts_fall_back_to_rounding():
    assert choose_tempo(np.array([10.0, 20.0]), np.ones(2), raw=124.02) == 124.0


# ---- excerpt choice ------------------------------------------------------------------

FPS = SR / HOP


def envelopes(sections, seconds=300.0, bpm=124.0):
    """Onset envelopes (t, env, low) for a track with kicks only inside `sections`
    ((start, end) s): a spike per beat there, hats (env only) everywhere."""
    t = np.arange(int(seconds * FPS)) / FPS
    beat = (np.abs(((t * bpm / 60) + 0.5) % 1 - 0.5) < 0.01).astype(float)
    kicks = np.zeros_like(t)
    for a, b in sections:
        kicks[(t >= a) & (t < b)] = 1.0
    low = beat * kicks
    env = low + 0.3 * (np.abs(((t * bpm / 60) % 1) - 0.5) < 0.01)
    return t, env, low


def test_excerpts_skip_intro_breakdown_and_outro():
    # 40 s with no kick, kicks to 120 s, a 30 s breakdown, kicks to 270 s, a fade.
    sections = [(40, 120), (150, 270)]
    t, env, low = envelopes(sections)
    starts = np.array(excerpt_starts(t, env, low, 44100)) / 44100
    assert len(starts) == EXCERPTS
    assert abs(starts[0] - 40) < 1.5  # the first excerpt starts with the kick
    assert abs(starts[-1] + EXCERPT_S - 270) < 1.5  # the last ends with it
    for s in starts:  # every excerpt lies inside a kick section
        assert any(a - 1.5 <= s and s + EXCERPT_S <= b + 1.5 for a, b in sections), s


def test_little_steady_kick_uses_the_whole_music():
    # Breakbeats: hardly any four-to-the-floor kick. Excerpts span the music as before.
    t, env, low = envelopes([(100, 160)])
    starts = np.array(excerpt_starts(t, env, low, 44100)) / 44100
    assert kick_sections(low, FPS).mean() < 0.3
    assert starts[0] < 5 and starts[-1] > 280


# ---- the flag ------------------------------------------------------------------------


def test_steady_when_the_excerpts_agree():
    assert is_steady(spread_ms=12, lined=5, excerpts=5, drift_pct=1.5)
    # One odd excerpt (a breakdown that slipped in) is tolerated...
    assert is_steady(spread_ms=12, lined=4, excerpts=5, drift_pct=1.5)


def test_flagged_when_the_excerpts_disagree():
    assert not is_steady(spread_ms=95, lined=5, excerpts=5, drift_pct=0.3)  # phase drift
    assert not is_steady(spread_ms=5, lined=2, excerpts=5, drift_pct=18)  # tempo change (126 -> 103)
    assert not is_steady(spread_ms=5, lined=3, excerpts=5, drift_pct=1.0)  # two don't line up
    assert not is_steady(spread_ms=5, lined=5, excerpts=5, drift_pct=7.0)  # one has its own tempo
