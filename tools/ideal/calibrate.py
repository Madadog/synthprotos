"""Calibrate the idealised piano model against what the fitted model learnt.

The fitted engine reproduces the recordings key by key (30 recorded keys x
16 velocity layers) and therefore inherits their quirks: every recorded key
is a separate sample with its own hammer, voicing and microphone colour, and
keys in between are blends.  The idealised engine instead uses a handful of
smooth laws over the whole keyboard.  This script finds their parameters by
one joint least-squares fit to the partial envelopes of the fitted model
(window-free and noise-free, see tools/fit), grouped into ERB bands and
weighted like the accuracy metric (tools/analysis/compare.py):

  amplitude   A_n = G(key, v)                          overall level (dB: cubic in
                                                       key, quadratic in dB(v))
                  * H(f_n; T(key, v), q(key))          hammer contact (low-pass)
                  * R(f_n)                             soundboard radiation
                  * S(n)                               strike-point comb
                  * n^-gamma(key)                      string slope
  envelope    a_n(t) = A_n ((1 - w2) e^{-r s_n t} + w2 e^{-s_n t})
              s_n = b1(key) + b3(key) f_n^2           (Bank & Valimaki)
              w2(key): aftersound share, r(key): prompt speed-up

R is the same for every key (one soundboard): a high-pass of order k below
its low corner (weak bass fundamentals) and a gentle low-pass at the top.
S = sqrt((sin^2(pi n beta) + eps^2) / (1/2 + eps^2)) is the strike-point comb
(beta = 1/8.5; the notch depth eps(key) is fitted).  All key dependences are low-order
polynomials in u = (key - 64) / 44, so the model is smooth across the whole
keyboard.

The shape of the laws is fitted in dB; afterwards the overall level is
shifted so that the laws carry the target's (A-weighted) energy over the
first 0.3 s, since a fit in dB lands on the geometric mean of the peaky band
levels, which is about 2-3 dB below their energy.

By default the laws are fitted directly to the recordings, with the same
objective as the per-key fit (tools/fit/fmfit.py: partial tracks of every
recorded key/layer averaged into ERB bands, analysis windows and the room
noise floor modelled, weights as in compare.py).  --source fitted fits them
to the fitted model's envelopes instead (noise-free, but its own errors then
add to the laws').

usage: python calibrate.py [--source recordings|fitted] [--init JSON] [--plots DIR] [--json OUT]
"""

from __future__ import annotations

import argparse
import json
import os
import pickle
import sys
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

import numpy as np
from scipy.optimize import least_squares

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "fit"))
import fmfit as F  # noqa: E402

LAYERS = [1, 4, 8, 12, 16]
# the frames of the accuracy metric (compare.py)
TIMES = np.array([0.01, 0.02, 0.035, 0.05, 0.075, 0.1, 0.15, 0.2, 0.3, 0.45, 0.65, 0.9,
                  1.2, 1.6, 2.1, 2.8, 3.6, 4.7, 6.0, 8.0])

# name, initial value, lower bound, upper bound
PARAMS = [
    ("g0", -10, -80, 40), ("g1", 0, -40, 40), ("g2", 0, -40, 40), ("g3", 0, -40, 40),
    ("d0", 0.8, 0.2, 4), ("d1", 0.3, -2, 2), ("d2", 0.3, -2, 2),
    ("t0", np.log(2e-4), np.log(1e-6), np.log(2e-2)), ("t1", 0, -4, 4), ("t2", 0, -4, 4),
    ("p0", 0.4, 0, 2), ("p1", -0.4, -1.5, 1.5),
    ("q0", 3.0, 0.5, 10), ("q1", 0, -4, 4),
    ("gam0", 1.0, -1, 3), ("gam1", 0, -2, 2),
    ("lflo", np.log(80.0), np.log(20), np.log(800)), ("lfhi", np.log(15000.0), np.log(500), np.log(20000)),
    ("bk", 1.5, 0.5, 8),
    ("c0", np.log(0.4), np.log(0.01), np.log(20)), ("c1", 1.2, -5, 5), ("c2", 0.1, -5, 5), ("c3", 0, -5, 5),
    ("e0", np.log(1e-7), np.log(1e-12), np.log(1e-3)), ("e1", -2, -20, 20), ("e2", 0, -20, 20),
    ("w0", -1.0, -8, 8), ("w1", 0, -8, 8), ("w2", 0, -8, 8), ("w3", 0, -8, 8),
    ("r0", np.log(3.0), np.log(0.05), np.log(200)), ("r1", 0, -5, 5), ("r2", 0, -5, 5), ("r3", 0, -5, 5),
    ("ce0", np.log(0.35), np.log(0.05), np.log(20)), ("ce1", 0, -10, 10),
    ("dv2", 0, -1, 1), ("pv2", 0, -2, 2),
]
# Strike point (fraction of the speaking length).  The notches it puts in the
# spectrum are clearly visible on the partial levels of the bass (A0-F#2 all
# show them at n = 8.4..8.8, about 6 dB deep at the nearest partials) but not
# higher up, so the notch depth eps(key) is fitted.
STRIKE = 1 / 8.5
NAMES = [p[0] for p in PARAMS]
IDX = {n: i for i, n in enumerate(NAMES)}


def P(th, *names):
    return [th[IDX[n]] for n in names]


def layer_velocity(layer):
    return (8 + (layer - 1) * (127 - 8) / 15) / 127


def u_of(m):
    return (m - 64.0) / 44.0


def a_weight_db(f):
    f2 = np.asarray(f, float) ** 2
    ra = (12194.0**2 * f2 * f2) / ((f2 + 20.6**2) * np.sqrt((f2 + 107.7**2) * (f2 + 737.9**2)) * (f2 + 12194.0**2))
    return 20 * np.log10(ra + 1e-30) + 2.0


def load_learnt():
    rows = []
    for p in sorted((ROOT / "tools/fit/results").glob("*.json")):
        d = json.loads(p.read_text())
        cs = d["centers"]
        for layer in LAYERS:
            L = d["layers"].get(str(layer))
            if L is None:
                continue
            f0, B = L["f0"], L["B"]
            n = np.arange(1, 91)
            f = n * f0 * np.sqrt(1 + B * n * n)
            keep = f < 15000
            n, f = n[keep], f[keep]
            Pw = F.model_power(np.asarray(L["x"]), cs, n, TIMES)
            G = F.erb_matrix(f)
            fb = (G @ f) / G.sum(axis=1)  # band centre (mean partial frequency)
            Lb = 10 * np.log10(G @ Pw + 1e-30)
            pk = Lb.max()
            rows.append(dict(midi=d["midi"], layer=layer, v=layer_velocity(layer), n=n, f=f, G=G,
                             Lb=np.maximum(Lb, pk - 70), aw=a_weight_db(fb)[:, None], pk=pk, P=Pw,
                             x=np.asarray(L["x"]), centers=cs))
    return rows


def _target(args):
    note, layer = args
    os.environ.setdefault("OMP_NUM_THREADS", "1")
    tg = F.make_target(note, layer)
    return dict(note=note, midi=tg.midi, layer=layer, n=tg.n, f=tg.f, t=tg.t, L=tg.L, noise=tg.noise,
                win_s=tg.win_s)


def load_recorded(cache: Path):
    """Partial-track targets of the recordings (cached: the analysis is slow)."""
    if cache.exists():
        tgs = pickle.loads(cache.read_bytes())
    else:
        sys.path.insert(0, str(ROOT / "tools" / "analysis"))
        import pianoan as pa

        jobs = [(note, layer) for note in pa.SALAMANDER_NOTES for layer in LAYERS]
        with ProcessPoolExecutor(max_workers=os.cpu_count()) as ex:
            tgs = list(ex.map(_target, jobs))
        cache.parent.mkdir(parents=True, exist_ok=True)
        cache.write_bytes(pickle.dumps(tgs))
    rows = []
    for tg in tgs:
        t, n, f = tg["t"], tg["n"], tg["f"]
        G = F.erb_matrix(f)
        fb = (G @ f) / G.sum(axis=1)
        Lp = np.maximum(tg["L"], tg["noise"][:, None])
        Lref = 10 * np.log10(G @ (10 ** (Lp / 10)))
        floor_pow = G @ (10 ** (tg["noise"] / 10))
        aw = a_weight_db(fb)[:, None]
        tt, ww = F.window_grid(t, tg["win_s"], K=12)
        rows.append(dict(kind="rec", midi=tg["midi"], layer=tg["layer"], v=layer_velocity(tg["layer"]), n=n, f=f,
                         t=t, G=G, Lref=Lref, floor=floor_pow[:, None], aw=aw, peakA=(Lref + aw).max(),
                         early=(t < 0.6 * tg["win_s"])[None, :], tt=tt, ww=ww))
    return rows


def windowed_power(th, r):
    """Partial power of the laws as the analysis sees it (window-averaged)."""
    A = amplitudes(th, r["midi"], r["v"], r["n"], r["f"])
    s, w2, rr = envelope_params(th, r["midi"], r["f"])
    tt = np.maximum(r["tt"], 0.0)[None, :, :]  # [1, t, K]
    on = (r["tt"] >= 0)[None, :, :]
    g1 = (np.where(on, np.exp(-(rr * s)[:, None, None] * tt), 0.0)) @ r["ww"]
    g2 = (np.where(on, np.exp(-s[:, None, None] * tt), 0.0)) @ r["ww"]
    a = A[:, None] * ((1 - w2) * g1 + w2 * g2)
    return a * a


def amplitudes(th, m, v, n, f):
    g0, g1, g2, g3, d0, d1, d2 = P(th, "g0", "g1", "g2", "g3", "d0", "d1", "d2")
    t0, t1, t2, p0, p1, q0, q1 = P(th, "t0", "t1", "t2", "p0", "p1", "q0", "q1")
    gam0, gam1, lflo, lfhi, bk, ce0, ce1 = P(th, "gam0", "gam1", "lflo", "lfhi", "bk", "ce0", "ce1")
    dv2, pv2 = P(th, "dv2", "pv2")
    u = u_of(m)
    V = 20 * np.log10(v)  # dB re. the loudest layer
    Gdb = g0 + g1 * u + g2 * u * u + g3 * u ** 3 + (d0 + d1 * u + d2 * u * u) * V + dv2 * V * V / 20
    lv = np.log(v)
    T = np.exp(t0 + t1 * u + t2 * u * u - (p0 + p1 * u) * lv + pv2 * lv * lv)
    H = (1 + (f * T) ** 2) ** (-(q0 + q1 * u) / 2)
    x = (f / np.exp(lflo)) ** (2 * bk)
    R = x / (1 + x) / np.sqrt(1 + (f / np.exp(lfhi)) ** 2)
    eps2 = np.exp(2 * (ce0 + ce1 * u))
    S = np.sqrt((np.sin(np.pi * n * STRIKE) ** 2 + eps2) / (0.5 + eps2))
    return 10 ** (Gdb / 20) * H * R * S * n ** (-(gam0 + gam1 * u))


def envelope_params(th, m, f):
    c0, c1, c2, c3, e0, e1, e2 = P(th, "c0", "c1", "c2", "c3", "e0", "e1", "e2")
    w0, w1, w2c, w3, r0, r1, r2, r3 = P(th, "w0", "w1", "w2", "w3", "r0", "r1", "r2", "r3")
    u = u_of(m)
    s = np.exp(c0 + c1 * u + c2 * u * u + c3 * u ** 3) + np.exp(e0 + e1 * u + e2 * u * u) * f * f
    w2 = 1 / (1 + np.exp(-(w0 + w1 * u + w2c * u * u + w3 * u ** 3)))
    r = 1 + np.exp(r0 + r1 * u + r2 * u * u + r3 * u ** 3)
    return s, w2, r


def model_power(th, m, v, n, f, t):
    A = amplitudes(th, m, v, n, f)
    s, w2, r = envelope_params(th, m, f)
    a = A[:, None] * ((1 - w2) * np.exp(-r * s[:, None] * t[None, :]) + w2 * np.exp(-s[:, None] * t[None, :]))
    return a * a


def row_error(th, r):
    """Weighted ERB-band error of one key/layer (weights as in compare.py, from
    the louder of model and target so that spurious loud energy counts)."""
    if r.get("kind") == "rec":
        Lm = 10 * np.log10(r["G"] @ windowed_power(th, r) + r["floor"])
        d = Lm - r["Lref"]
        # frames whose window still overlaps the onset only bound from above
        d = np.where(r["early"], np.maximum(d - 3.0, 0.0), d)
        LA = np.maximum(Lm, r["Lref"]) + r["aw"]
        w = np.clip((LA - (r["peakA"] - 65)) / 45, 0.02, 1.0)
        return d * w, w
    Pm = model_power(th, r["midi"], r["v"], r["n"], r["f"], TIMES)
    Lm = np.maximum(10 * np.log10(r["G"] @ Pm + 1e-30), r["pk"] - 70)
    LA = np.maximum(Lm, r["Lb"]) + r["aw"]
    peak = (r["Lb"] + r["aw"]).max()
    w = np.clip((LA - (peak - 65)) / 45, 0.02, 1.0)
    return (Lm - r["Lb"]) * w, w


def fit(rows, x0=None):
    def resid(th):
        out = []
        for r in rows:
            e, w = row_error(th, r)
            out.append(e.ravel() / np.sqrt((w * w).sum()))
        return np.concatenate(out)

    if x0 is None:
        x0 = np.array([p[1] for p in PARAMS], float)
    lo = [p[2] for p in PARAMS]
    hi = [p[3] for p in PARAMS]
    r = least_squares(resid, x0, bounds=(lo, hi), x_scale="jac", max_nfev=4000)
    return r.x, float(np.sqrt(np.sum(r.fun ** 2) / len(rows)))


def energy_offset(th, rows, t_max=0.3):
    """Mean A-weighted energy difference (dB, laws minus target) over the
    first t_max seconds.  A fit in dB lands on the geometric mean of the
    target's peaky band levels, which lies below its energy (loudness)."""
    out = []
    for r in rows:
        if r.get("kind") == "rec":
            Pm, Pt, t = r["G"] @ windowed_power(th, r), 10 ** (r["Lref"] / 10), r["t"]
        else:
            Pm = r["G"] @ model_power(th, r["midi"], r["v"], r["n"], r["f"], TIMES)
            Pt, t = 10 ** (r["Lb"] / 10), TIMES
        aw = 10 ** (r["aw"] / 10)
        sel = t < t_max
        out.append(10 * np.log10((Pm[:, sel] * aw).sum() / (Pt[:, sel] * aw).sum()))
    return float(np.mean(out))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--plots", type=Path)
    ap.add_argument("--json", type=Path)
    ap.add_argument("--init", type=Path, help="start from a previous calibration (JSON)")
    ap.add_argument("--source", choices=["recordings", "fitted"], default="recordings")
    ap.add_argument("--cache", type=Path, default=ROOT / "out" / "ideal_targets.pkl",
                    help="cache of the analysed recordings")
    a = ap.parse_args()
    rows = load_recorded(a.cache) if a.source == "recordings" else load_learnt()
    x0 = None
    if a.init:
        x0 = np.array([p[1] for p in PARAMS], float)
        for k, v in json.loads(a.init.read_text())["params"].items():
            if k in IDX:
                x0[IDX[k]] = np.clip(v, PARAMS[IDX[k]][2], PARAMS[IDX[k]][3])
    th, err = fit(rows, x0)
    # the shape is fitted in dB; the overall level then matches the energy
    off = energy_offset(th, rows)
    th[IDX["g0"]] -= off
    print(f"level shifted by {-off:+.2f} dB to match the target's energy over the first 0.3 s")
    print(f"joint fit: weighted rms {err:.2f} dB over {len(rows)} key/layer envelopes ({len(TIMES)} times, ERB bands)")
    for lo, hi in [(21, 33), (36, 57), (60, 81), (84, 108)]:
        es = [np.sqrt(((row_error(th, r)[0]) ** 2).sum() / (row_error(th, r)[1] ** 2).sum())
              for r in rows if lo <= r["midi"] <= hi]
        print(f"  keys {lo}-{hi}: {np.mean(es):.2f} dB")
    for k, v in zip(NAMES, th):
        print(f"  {k:6s} = {v: .6g}")
    out = dict(params=dict(zip(NAMES, map(float, th))), rms_db=err, source=a.source, level_shift_db=-off)
    if a.json:
        a.json.write_text(json.dumps(out, indent=1))
    if a.plots:
        import matplotlib

        matplotlib.use("Agg")
        import matplotlib.pyplot as plt

        a.plots.mkdir(parents=True, exist_ok=True)
        keys = [21, 36, 48, 60, 72, 84, 96]
        fig, axes = plt.subplots(2, len(keys), figsize=(3.2 * len(keys), 6), squeeze=False)
        for j, key in enumerate(keys):
            for i, layer in enumerate([4, 16]):
                ax = axes[i][j]
                r = min((r for r in rows if r["layer"] == layer), key=lambda r: abs(r["midi"] - key))
                fb = (r["G"] @ r["f"]) / r["G"].sum(axis=1)
                if r.get("kind") == "rec":
                    t, Lt = r["t"], r["Lref"]
                    Lm = 10 * np.log10(r["G"] @ windowed_power(th, r) + r["floor"])
                else:
                    t, Lt = TIMES, r["Lb"]
                    Lm = 10 * np.log10(r["G"] @ model_power(th, r["midi"], r["v"], r["n"], r["f"], TIMES) + 1e-30)
                for tt, col in [(0.05, "k"), (0.3, "b"), (1.2, "g"), (3.6, "r")]:
                    ti = int(np.argmin(np.abs(t - tt)))
                    ax.semilogx(fb, Lt[:, ti], col + "-", lw=0.6, alpha=0.5)
                    ax.semilogx(fb, Lm[:, ti], col + "-", lw=1.3, label=f"{t[ti]:.2g} s")
                ax.set_title(f"key {r['midi']} layer {layer}", fontsize=8)
                ax.set_ylim(Lt.max() - 70, Lt.max() + 6)
                ax.tick_params(labelsize=7)
                if i == 0 and j == 0:
                    ax.legend(fontsize=6)
        src = "recordings" if rows[0].get("kind") == "rec" else "fitted model"
        fig.suptitle(f"ERB-band levels vs frequency (Hz). thin: {src}; thick: smooth laws", fontsize=9)
        fig.tight_layout()
        fig.savefig(a.plots / "ideal_laws.png", dpi=70)
        plt.close(fig)


if __name__ == "__main__":
    main()
