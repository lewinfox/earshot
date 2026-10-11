"""Zoomed waveform + grid lines, as PNG (run with the plotting venv, see README).

usage: plotvenv/bin/python -I plots.py <out_dir> <id> [<id> ...] [--grids a,b,c] [--span 1.2]

One image per track: three panels (start, middle, end), each a short stretch around a
ground-truth beat with a strong kick. Full signal in light grey, kick band (low-pass
150 Hz) in dark grey. One row of tick marks per grid at the top, each in its own
colour, with a thin line down through the waveform.

Grids are read from DATA/grids/<name>/<id>.npy (beat times, s); "truth" is the
refined full Beat This! beats.
"""
from __future__ import annotations

import sys

sys.path.insert(0, __file__.rsplit("/", 1)[0])
import matplotlib  # noqa: E402

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402
import numpy as np  # noqa: E402
import soundfile as sf  # noqa: E402
from scipy import signal  # noqa: E402

import gtlib  # noqa: E402
from common import DATA, wav_path  # noqa: E402

COLOURS = {"truth": "#1a9850", "attack": "#000000", "hybrid": "#e7298a", "fast_rs": "#d7301f", "fast": "#f768a1", "static": "#2166ac", "static_snap": "#7b3294", "small_line": "#e08214", "full_line": "#01665e", "db": "#7f7f7f"}
LABELS = {"truth": "Beat This! full", "attack": "hit start (reference)", "hybrid": "H: hybrid (recommended)", "fast_rs": "A: Rust DSP", "fast": "A (Python)", "static": "static (today)", "static_snap": "B: static + snap", "small_line": "C: small model + line", "full_line": "D: full model + line", "db": "grid in app DB"}


def load_grid(name: str, tid: int) -> np.ndarray | None:
    if name == "truth":
        return gtlib.truth(tid).beats
    p = DATA / "grids" / name / f"{tid}.npy"
    return np.load(p) if p.exists() else None


def pick_spots(beats, lp, sr, duration, fracs=(0.08, 0.5, 0.92)):
    spots = []
    for f in fracs:
        c = f * duration
        cand = beats[(beats > c - 8) & (beats < c + 8) & (beats > 1) & (beats < duration - 2)]
        if not len(cand):
            spots.append(c)
            continue
        e = [np.abs(lp[int(t * sr) : int((t + 0.15) * sr)]).max() for t in cand]
        spots.append(float(cand[int(np.argmax(e))]))
    return spots


def render(tid: int, out: str, names: list[str], span: float = 1.2, spots=None, title="") -> None:
    y, sr = sf.read(wav_path(tid), dtype="float32")
    lp = signal.sosfiltfilt(signal.butter(4, 150, "low", fs=sr, output="sos"), y)
    gt = gtlib.truth(tid)
    grids = [(n, load_grid(n, tid)) for n in names]
    grids = [(n, g) for n, g in grids if g is not None]
    spots = spots or pick_spots(gt.beats, lp, sr, gt.duration)
    fig, axes = plt.subplots(1, len(spots), figsize=(5.2 * len(spots), 3.0), dpi=110)
    axes = np.atleast_1d(axes)
    lanes = len(grids)
    for ax, c in zip(axes, spots):
        t0, t1 = c - 0.35 * span, c + 0.65 * span
        a, b = int(max(0, t0) * sr), int(t1 * sr)
        t = np.arange(a, b) / sr
        seg = y[a:b] / (np.abs(y[a:b]).max() + 1e-9)
        lseg = lp[a:b] / (np.abs(lp[a:b]).max() + 1e-9)
        ax.fill_between(t, -np.abs(seg), np.abs(seg), color="#c8c8c8", lw=0)
        ax.plot(t, lseg, color="#404040", lw=0.6)
        for k, (n, g) in enumerate(grids):
            top = 1.0 + (lanes - k) * 0.22
            for bt in g[(g >= t0) & (g <= t1)]:
                ax.plot([bt, bt], [-1.05, top], color=COLOURS.get(n, "k"), lw=0.9, alpha=0.75)
                ax.plot([bt], [top], marker="v", color=COLOURS.get(n, "k"), ms=6)
        ax.set_xlim(t0, t1)
        ax.set_ylim(-1.1, 1.15 + lanes * 0.22)
        ax.set_yticks([])
        ax.xaxis.set_major_locator(matplotlib.ticker.MultipleLocator(0.1))
        ax.xaxis.set_minor_locator(matplotlib.ticker.MultipleLocator(0.01))
        ax.tick_params(axis="x", labelsize=7)
        ax.grid(axis="x", which="major", color="#eeeeee", lw=0.6)
        ax.set_title(f"{c / gt.duration:.0%} through ({c:.1f} s)", fontsize=8)
    handles = [plt.Line2D([], [], color=COLOURS.get(n, "k"), marker="v", lw=1, label=LABELS.get(n, n)) for n, _ in grids]
    handles.append(plt.Line2D([], [], color="#404040", lw=1, label="kick band (<150 Hz)"))
    fig.legend(handles=handles, loc="lower center", ncol=len(handles), fontsize=7, frameon=False)
    fig.suptitle(title or f"track {tid}", fontsize=9)
    fig.tight_layout(rect=(0, 0.07, 1, 0.95))
    fig.savefig(out)
    plt.close(fig)


def render_drift(tid: int, out: str, title: str = "") -> None:
    """Two strips over the whole track: how far each Beat This! beat sits from the
    straight-line grid (ms), and the local tempo (BPM over 8 beats)."""
    gt = gtlib.truth(tid)
    fig, (a1, a2) = plt.subplots(2, 1, figsize=(10, 3.4), dpi=100, sharex=True)
    r = gt.resid * 1000
    a1.plot(gt.beats, r, ".", ms=2, color="#1a9850")
    lim = max(60, min(400, np.percentile(np.abs(r), 98) * 1.2))
    a1.set_ylim(-lim, lim)
    a1.axhline(0, color="#999", lw=0.6)
    for y in (-20, 20):
        a1.axhline(y, color="#ccc", lw=0.6, ls="--")
    a1.set_ylabel("off straight\ngrid (ms)", fontsize=7)
    a2.plot(gt.lt, gt.lbpm, color="#2166ac", lw=0.8)
    a2.axhline(gt.bpm, color="#999", lw=0.6)
    a2.set_ylabel("local BPM", fontsize=7)
    a2.set_xlabel("time (s)", fontsize=7)
    for a in (a1, a2):
        a.tick_params(labelsize=7)
    fig.suptitle(title or f"track {tid}: Beat This! beats vs one straight grid at {gt.bpm:.2f} BPM", fontsize=9)
    fig.tight_layout()
    fig.savefig(out)
    plt.close(fig)


if __name__ == "__main__":
    if sys.argv[1] == "--drift":
        for a in sys.argv[3:]:
            render_drift(int(a), f"{sys.argv[2]}/drift_{a}.png")
            print(f"{sys.argv[2]}/drift_{a}.png")
        sys.exit()
    args = sys.argv[1:]
    names = ["truth", "fast"]
    span = 1.2
    if "--grids" in args:
        i = args.index("--grids")
        names = args[i + 1].split(",")
        del args[i : i + 2]
    if "--span" in args:
        i = args.index("--span")
        span = float(args[i + 1])
        del args[i : i + 2]
    out_dir = args[0]
    for a in args[1:]:
        render(int(a), f"{out_dir}/{a}.png", names, span)
        print(f"{out_dir}/{a}.png")
