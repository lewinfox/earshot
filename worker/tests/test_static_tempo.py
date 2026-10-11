"""choose_tempo: the tempo from beat times found in excerpts across a track."""

import numpy as np

from earshot_worker.static_tempo import STEADY_MS, choose_tempo, grid_fit

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
