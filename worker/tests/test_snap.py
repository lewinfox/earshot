"""snap: moving a straight grid to where the kicks start."""

import numpy as np

from earshot_worker.snap import attacks, energy, kick_shifts, snap_to_kicks

SR = 44100
BPM = 124.0
PERIOD = 60 / BPM


def kick(sr=SR):
    """A kick: a falling sine from 150 to 45 Hz, starting at full level."""
    n = int(0.25 * sr)
    tt = np.arange(n) / sr
    f = 150 * np.exp(-tt * 30) + 45
    return (np.sin(2 * np.pi * np.cumsum(f) / sr) * np.exp(-tt / 0.08) * 0.9).astype(np.float32)


def noise(ms, level, seed=0, sr=SR):
    return (np.random.default_rng(seed).normal(0, level, int(ms / 1000 * sr))).astype(np.float32)


def track(hit, seconds=40.0, first=0.5, sr=SR):
    """`hit` (a signal) placed so that it starts on every beat from `first`, over quiet
    noise. Returns the signal and the beat times."""
    y = noise(seconds * 1000, 0.002, seed=1)
    times = first + PERIOD * np.arange(int((seconds - first - 0.5) / PERIOD))
    for t in times:
        i = int(round(t * sr))
        y[i : i + len(hit)] += hit[: len(y) - i]
    return y, times


def test_attack_on_a_plain_kick():
    y, times = track(kick())
    at, ok = attacks(times[2:-2] + 0.030, energy(y, SR), 0.060, 0.040)  # lines 30 ms late
    assert ok.all()
    assert np.abs(at - times[2:-2]).max() < 0.002


def test_faint_sound_before_the_click_is_ignored():
    # A soft noise swell 15 ms before each kick (15% of its level): the grid belongs
    # on the kick, not on that.
    pre = noise(15, 0.13, seed=2)
    hit = np.concatenate([pre, kick()])
    y, starts = track(hit)
    kicks = starts + len(pre) / SR
    at, ok = attacks(kicks[2:-2] + 0.030, energy(y, SR), 0.060, 0.040)
    assert ok.all()
    assert np.abs(at - kicks[2:-2]).max() < 0.002


def test_click_before_the_low_thump_counts():
    # A loud click, then the kick's body 20 ms later: the hit starts at the click.
    click = noise(3, 0.5, seed=3)
    hit = np.concatenate([click, np.zeros(int(0.017 * SR), np.float32), kick()])
    y, starts = track(hit)
    at, ok = attacks(starts[2:-2] + 0.030, energy(y, SR), 0.060, 0.040)
    assert ok.all()
    # The odd hit can still come out on the body; the snap takes the median.
    err = np.abs(at - starts[2:-2])
    assert np.median(err) < 0.003 and np.mean(err < 0.003) > 0.9


def test_snap_moves_a_late_grid_onto_the_kicks():
    y, times = track(kick(), seconds=60.0)
    late = times[0] + 0.032  # as Beat This! leaves it
    shift = snap_to_kicks(y, SR, BPM, late)
    assert abs(late + shift - times[0]) < 0.002
    # Not wrapped: a downbeat moved by the same amount stays on its beat.
    assert -0.040 < shift < 0


def drifting_track(seconds=90.0, first=0.5, sr=SR):
    """Kicks whose tempo drifts from 122 to 126 BPM, over quiet noise. Returns the
    signal and the kick start times."""
    y = noise(seconds * 1000, 0.002, seed=4)
    times, t = [], first
    while t < seconds - 0.5:
        times.append(t)
        t += 60 / (122 + 4 * t / seconds)
    hit = kick(sr)
    for t in times:
        i = int(round(t * sr))
        y[i : i + len(hit)] += hit[: len(y) - i]
    return y, np.array(times)


def test_dynamic_grid_lands_on_the_kicks():
    # A beat per kick, as Beat This! leaves them: 22 ms late in the first half, 36 ms
    # late in the second, with a few ms of jitter. Each stretch moves by its own offset.
    y, times = drifting_track()
    late = np.where(times < 45, 0.022, 0.036)
    lines = times + late + np.random.default_rng(5).uniform(-0.003, 0.003, len(times))
    snapped = lines + kick_shifts(y, SR, lines, window=32)
    err = np.abs(snapped - times)[2:-2]
    assert np.median(err) < 0.003 and np.percentile(err, 90) < 0.006, err
    # A straight-grid snap can't follow the change in lateness.
    one = lines + kick_shifts(y, SR, lines)
    assert np.percentile(np.abs(one - times)[2:-2], 90) > 0.005


def test_too_few_hits_leave_the_grid_alone():
    y = noise(30_000, 0.002, seed=6)
    lines = 0.5 + PERIOD * np.arange(50)
    assert not kick_shifts(y, SR, lines, window=32).any()
