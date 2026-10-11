"""Build the local review page: a few tracks where the call matters, with zoomed
waveform + grid images and a verdict for each. Run with ./pp (the plotting venv).

usage: ./pp make_review.py      writes DATA/review/index.html (+ PNGs beside it)
"""
from __future__ import annotations

import html
import json
import sys

sys.path.insert(0, __file__.rsplit("/", 1)[0])
import numpy as np  # noqa: E402

import gtlib  # noqa: E402
import plots  # noqa: E402
from common import DATA, title  # noqa: E402

OUT = DATA / "review"
OUT.mkdir(exist_ok=True)
GRIDS = ["truth", "attack", "static", "static_snap", "full_line", "fast"]
scores = json.loads((DATA / "results" / "scores.json").read_text())


def result(method: str, tid: int) -> dict | None:
    p = DATA / "results" / f"{method}.jsonl"
    found = None
    for line in p.read_text().splitlines() if p.exists() else []:
        r = json.loads(line)
        if r["id"] == tid:
            found = r
    return found


def numbers(tid: int) -> str:
    """Per method: tempo and median distance from the hit-start line (ms)."""
    gt = gtlib.truth(tid)
    rows = [f"<tr><td>{plots.LABELS['truth']}</td><td>{gt.bpm:.2f}</td><td>{np.median((gt.beats - gt.ref_line) * 1000):+.0f}</td></tr>"]
    for m in GRIDS[2:]:
        r = result(m, tid)
        if not r or "bpm" not in r:
            if r:
                rows.append(f"<tr><td>{plots.LABELS[m]}</td><td colspan=2>failed: {html.escape(r.get('failed', ''))}</td></tr>")
            continue
        g = np.load(DATA / "grids" / m / f"{tid}.npy")
        s = gtlib.score(gt, g, r["bpm"], "line")
        rows.append(f"<tr><td>{plots.LABELS[m]}</td><td>{r['bpm']:.2f}</td><td>{s['bias_ms']:+.0f}</td></tr>")
    return "<table><tr><th>method</th><th>BPM</th><th>vs hit start, ms (median; + = late)</th></tr>" + "".join(rows) + "</table>"


EXAMPLES = [
    dict(
        id=82,
        kind="regular",
        verdict="Typical regular track. Beat This! (green) and today's static grid (blue) sit about 30 ms after the start of the kick. "
        "B (static + snap, purple) and D (full model + line, teal) sit on the start of the hit (black).",
        question="Is the black line (where the sound of the hit first rises) where you want grid lines?",
    ),
    dict(
        id=19,
        kind="regular",
        verdict="The hardest regular track in the sample. The hit opens with a short click, and the kick's low thump (dark wave) follows "
        "about 20 ms later. Beat This! is on the click. The snapped grids (B, D, A) land 10-20 ms before the click, because the "
        "start of the hit varies from beat to beat here (some beats have a soft sound just before). I think they are early here.",
        question="Where should the grid sit on this track: the click, the start of the low thump, or the first faint sound before them?",
    ),
    dict(
        id=54,
        kind="regular",
        verdict="Clean kick with silence before it. The snapped grids (B, D) sit on the start of the kick, within a few ms of the reference. "
        "Beat This! and today's static grid are about 40 ms late.",
        question="Is this the right place (start of the kick, not its loudest point)?",
    ),
    dict(
        id=468,
        kind="irregular",
        verdict="Tempo ramps from 126 to 103 BPM between 75 s and 115 s. No straight grid fits. Beat This! follows it; "
        "the proposed flag (beats far from one straight line) catches it easily.",
        question=None,
        drift=True,
    ),
    dict(
        id=1035,
        kind="irregular?",
        verdict="Straight at 128 BPM except for about 15 s around 200 s, where Beat This!'s beats sit 130 ms off the line. "
        "In the middle image the straight grids land on the strong hit at 204.4 s and Beat This! doesn't, so I think Beat This! lost "
        "the beat in a busy patch and the track is really regular. My rule labels it irregular because it trusts Beat This!.",
        question="Is this track regular (Beat This! wrong around 200 s) or does it really jump there? The middle images are at the odd patch.",
        drift=True,
        spots=[100.0, 204.0, 300.0],
    ),
    dict(
        id=1018,
        kind="irregular?",
        verdict="The first 30 s sit 40-50 ms early against the straight line through the rest, then it wanders by about 20 ms. "
        "Looks like an edit with a spliced-on intro. The straight grids disagree by up to 70 ms here, and B and A sit about 40 ms "
        "before the kick in the middle and end images. I'd flag it.",
        question="Should a track like this be flagged for straightening, or is it close enough to regular?",
        drift=True,
        spots=[12.0, 100.0, 180.0],
    ),
]


def nearest_beat(tid: int, t: float) -> float:
    b = gtlib.truth(tid).beats
    return float(b[np.argmin(np.abs(b - t))])


def main():
    cards = []
    for ex in EXAMPLES:
        tid = ex["id"]
        spots = [nearest_beat(tid, t) for t in ex["spots"]] if "spots" in ex else None
        plots.render(tid, str(OUT / f"{tid}_zoom.png"), GRIDS, 0.3, spots=spots, title=f"{tid}: zoomed to 0.3 s around a beat")
        plots.render(tid, str(OUT / f"{tid}_wide.png"), GRIDS, 1.2, spots=spots, title=f"{tid}: 1.2 s around the same beats")
        imgs = [f"{tid}_zoom.png", f"{tid}_wide.png"]
        if ex.get("drift"):
            plots.render_drift(tid, str(OUT / f"{tid}_drift.png"))
            imgs.insert(0, f"{tid}_drift.png")
        q = f'<p class="q"><b>Question:</b> {html.escape(ex["question"])}</p>' if ex["question"] else ""
        cards.append(
            f'<section><h2>{tid}: {html.escape(title(tid))} <span class="tag">{ex["kind"]}</span></h2>'
            f'<p><b>Verdict:</b> {html.escape(ex["verdict"])}</p>{q}{numbers(tid)}'
            + "".join(f'<img src="{i}" alt="track {tid} {i}">' for i in imgs)
            + "</section>"
        )
    page = f"""<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Beat grid review</title><style>
:root{{--bg:#fff;--fg:#1d1d1f;--muted:#666;--line:#ddd;--card:#fafafa}}
@media (prefers-color-scheme: dark){{:root:not([data-theme="light"]){{--bg:#161616;--fg:#eee;--muted:#aaa;--line:#333;--card:#1f1f1f}}}}
:root[data-theme="dark"]{{--bg:#161616;--fg:#eee;--muted:#aaa;--line:#333;--card:#1f1f1f}}
body{{background:var(--bg);color:var(--fg);font:15px/1.5 system-ui,sans-serif;max-width:1100px;margin:0 auto;padding:16px}}
section{{background:var(--card);border:1px solid var(--line);border-radius:8px;padding:12px 16px;margin:20px 0}}
h2{{font-size:17px;margin:4px 0 8px}} .tag{{font-size:12px;color:var(--muted);border:1px solid var(--line);border-radius:4px;padding:1px 6px;margin-left:6px}}
img{{width:100%;height:auto;background:#fff;border-radius:4px;margin:6px 0}} .q{{border-left:3px solid #e08214;padding-left:10px}}
table{{border-collapse:collapse;font-size:13px;margin:6px 0}} td,th{{border-bottom:1px solid var(--line);padding:2px 10px;text-align:left}}
.key span{{display:inline-block;margin-right:14px}}
</style></head><body>
<h1>Beat grid review</h1>
<p>Six tracks where the call matters. In each image: grey is the full waveform, the dark line is the kick band (below 150 Hz).
Coloured markers are grid lines. The 0.3 s images show one beat; the 1.2 s images show two or three.</p>
<p class="key"><span style="color:#1a9850">&#9660; Beat This! full (whole track, the model)</span>
<span>&#9660; hit start: where the sound of each hit first rises (my reference)</span>
<span style="color:#2166ac">&#9660; static: today's default</span>
<span style="color:#7b3294">&#9660; B: static + snap to hit start</span>
<span style="color:#01665e">&#9660; D: full model + straight line + snap</span>
<span style="color:#f768a1">&#9660; A: no model, signal processing only</span></p>
{"".join(cards)}
</body></html>"""
    (OUT / "index.html").write_text(page)
    print(OUT / "index.html")


if __name__ == "__main__":
    main()
