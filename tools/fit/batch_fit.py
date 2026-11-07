"""Fit the banded-FM model to every reference note and velocity layer.

usage: python batch_fit.py OUTDIR NOTE [NOTE ...]

For each note the loudest layer (v16) is fitted from scratch with restarts,
then the softer layers are fitted by continuation (initialised from, and
gently regularised towards, the next louder layer) so that parameters vary
smoothly with velocity.
"""

import json
import sys
import time
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
import fmfit as F  # noqa: E402

# Fitted velocity layers; the others are interpolated by export_tables.py.
LAYERS = [16, 14, 12, 10, 8, 6, 4, 2, 1]


def fit_note(note, outdir: Path):
    t_start = time.time()
    res = {"note": note, "midi": F.pa.note_to_midi(note), "layers": {}}
    centers = None
    prev = None
    for v in LAYERS:
        tg = F.make_target(note, v)
        if centers is None:
            centers = F.default_centers(tg)
            res["centers"] = centers
        if prev is None:
            r = F.fit(tg, centers=centers, restarts=4)
        else:
            r = F.fit(tg, centers=centers, x0=prev, restarts=1, prior=prev, prior_w=0.15, max_nfev=250)
        prev = r["x"]
        part = F.evaluate(tg, r["x"], centers, erb=False)["rms_db"]
        res["caps"] = r["caps"]
        res["layers"][v] = {
            "x": [float(a) for a in r["x"]],
            "band_err": r["rms_db"],
            "partial_err": part,
            "f0": tg.f0,
            "B": tg.B,
            "cents": tg.analysis.pitch.cents,
            "peak_db": tg.peak,
            "rms_peak_db": float(tg.analysis.peak_db),
        }
        print(f"{note} v{v:2d} band {r['rms_db']:.2f} dB  partial {part:.2f} dB", flush=True)
    res["seconds"] = time.time() - t_start
    (outdir / f"{note}.json").write_text(json.dumps(res, indent=1))


if __name__ == "__main__":
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    for note in sys.argv[2:]:
        fit_note(note, out)
