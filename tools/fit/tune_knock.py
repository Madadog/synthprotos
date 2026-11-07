"""Tune the hammer-noise ("knock"/"thump") model against the recordings.

Below the fundamental of the top two octaves, and in the wide gaps between
their partials, the recordings contain only the impact noise of hammer,
action and soundboard, so the FM noise generators can be fitted there in
isolation.  The engine is rendered through the CLI
with parameter overrides (FMPIANO_KNOCK="name=value,...") and a simple
coordinate search minimises the RMS level difference in octave bands over
time.

usage: python tune_knock.py SCRATCH_DIR [thump]   (needs the samples, see README)

Without "thump" the sympathetic halo is tuned (with the thump/click at
their current defaults); with it, the thump and click.
"""

import json
import os
import subprocess
import sys
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "analysis"))
import pianoan as pa  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
CLI = REPO / "target/release/fmpiano"
NOTES = ["C5", "F#5", "A5", "C6", "D#6", "F#6", "A6", "C7", "D#7", "F#7", "A7", "C8"]
LAYERS = [4, 10, 16]
FRAMES = np.array([0.006, 0.015, 0.03, 0.06, 0.1, 0.2, 0.4, 0.8, 1.4])
# third-octave bands 25 Hz .. 12.5 kHz
BANDS = [(25 * 2 ** (k / 3), 25 * 2 ** ((k + 1) / 3)) for k in range(27)]
W = 1024


def noise_bands(f0, B):
    """Bands that contain no partial (with a margin): only knock lives there."""
    fn = pa.partial_freqs(f0, B, np.arange(1, 12))
    ok = []
    for lo, hi in BANDS:
        clear = not np.any((fn > lo / 1.15) & (fn < hi * 1.15))
        ok.append(clear and hi < 12500)
    return np.array(ok)


def band_levels(x, sr, f0, B=0.0):
    on = pa.find_onset(x, sr)
    w = np.hanning(W)
    use = noise_bands(f0, B)
    out = np.full((len(FRAMES), len(BANDS)), np.nan)
    for i, t in enumerate(FRAMES):
        c = on + int(t * sr)
        seg = x[max(0, c - W // 2) : c + W // 2]
        if len(seg) < W:
            seg = np.pad(seg, (W - len(seg), 0))
        X = np.abs(np.fft.rfft(seg * w, 4096)) * 2 / w.sum()
        f = np.arange(len(X)) * sr / 4096
        for j, (lo, hi) in enumerate(BANDS):
            if use[j]:
                out[i, j] = 10 * np.log10((X[(f >= lo) & (f < hi)] ** 2).sum() + 1e-20)
    return out


_ref = {}


def ref_levels():
    if not _ref:
        for n in NOTES:
            for layer in LAYERS:
                x, sr = pa.load(pa.ref_path(n, layer))
                fit = pa.estimate_f0_B(x, sr, pa.note_to_midi(n), pa.find_onset(x, sr))
                _ref[(n, layer)] = (band_levels(x, sr, fit.f0, fit.B), fit)
    return _ref


def evaluate(params, scratch: Path):
    env = dict(os.environ, FMPIANO_KNOCK=",".join(f"{k}={v}" for k, v in params.items()))
    d = scratch / "knock"
    subprocess.run([str(CLI), "refset", "--outdir", str(d), "--layers", "4,10,16", "--dur", "1.6"],
                   env=env, check=True, capture_output=True)
    errs = []
    for n in NOTES:
        for layer in LAYERS:
            x, sr = pa.load(d / f"{n}v{layer}.wav")
            r, fit = ref_levels()[(n, layer)]
            s = band_levels(x, sr, fit.f0, fit.B)
            m = np.isfinite(r) & (r > -95)  # above the recordings' room noise
            errs.append(np.where(m, np.maximum(s, -95) - r, np.nan))
    E = np.array(errs)
    return float(np.sqrt(np.nanmean(E**2))), float(np.nanmean(E))


def main():
    scratch = Path(sys.argv[1])
    # start from the current defaults; this pass tunes the sympathetic halo
    p = dict(halo_db=-30.0, halo_key_db=25.0, halo_vel_db=12.0, halo_tau=0.2, halo_fc=2500.0,
             halo_fm=1700.0, halo_i0=2.5, halo_lp=5000.0)
    if len(sys.argv) > 2 and sys.argv[2] == "thump":
        p = dict(thump_db=-26.8, thump_key_db=15.6, thump_vel_db=12.0, thump_tau=0.074, tail_rel_db=-17.8,
                 tail_tau=0.98, thump_fc=64.0, thump_fm=149.0, thump_i0=7.9, thump_rise=0.0096, thump_lp=400.0,
                 click_db=-30.0, click_fc=1500.0, click_i0=3.0, click_tau=0.0025)
    steps = {k: (4.0 if k.endswith("_db") else 0.4) for k in p}
    mult = {k for k in p if not k.endswith("_db")}
    best, _ = evaluate(p, scratch)
    print("start", round(best, 3))
    for rnd in range(3):
        for k in steps:
            for sgn in (1, -1):
                q = dict(p)
                q[k] = p[k] * (1 + sgn * steps[k]) if k in mult else p[k] + sgn * steps[k]
                e, b = evaluate(q, scratch)
                if e < best - 0.02:
                    best, p = e, q
                    print(f"  {k} -> {q[k]:.4g}: {e:.3f} dB (bias {b:+.2f})", flush=True)
                    break
        for k in steps:
            steps[k] *= 0.6
        print("round", rnd, round(best, 3), json.dumps({k: round(v, 4) for k, v in p.items()}), flush=True)




# --------------------------------------------------------------------------
# Halo tuning on the full ERB-band spectrogram error of the top keys (the
# halo hugs the fundamental, so the partial-free bands above cannot see it).
HALO_NOTES = ["A5", "C6", "D#6", "F#6", "A6", "C7", "D#7", "F#7", "A7", "C8"]
_halo_ref = {}


def _band_err(ra, x, sr):
    import compare as C  # tools/analysis

    sa = pa.analyse_signal(x, sr, ra.midi, pitch=ra.pitch, max_dur=3.0)
    rt, st = ra.tracks, sa.tracks
    nb = min(rt.erb_db.shape[1], st.erb_db.shape[1], int(np.searchsorted(rt.erb_f, 15000.0)))
    frames = C.FRAMES[C.FRAMES < min(rt.t[-1], st.t[-1]) - 0.1]
    R = C.smoothed(rt.t, rt.erb_db[:, :nb], frames)
    S = C.smoothed(st.t, st.erb_db[:, :nb], frames)
    win = int(np.clip(6.0 / rt.f0, 0.016, 0.25) * sr) // 2 * 2
    fl = np.maximum(C.band_noise(rt.erb_f[:nb], win)[None, :], R.max() - 70.0)
    Rc, Sc = np.maximum(R, fl), np.maximum(S, fl)
    RA = Rc + C.a_weight_db(rt.erb_f[:nb])[None, :]
    w = np.clip((RA - (RA.max() - 65)) / 45, 0.02, 1.0) * ((Rc > fl + 3) | (Sc > fl + 3))
    return ((Sc - Rc) ** 2 * w**2).sum(), (w**2).sum(), ((Sc - Rc) * w**2).sum()


def evaluate_halo(params, scratch: Path):
    sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "analysis"))
    env = dict(os.environ, FMPIANO_KNOCK=",".join(f"{k}={v}" for k, v in params.items()))
    d = scratch / "halo"
    subprocess.run([str(CLI), "refset", "--outdir", str(d), "--layers", "4,10,16", "--dur", "3.2"],
                   env=env, check=True, capture_output=True)
    num = den = bias = 0.0
    for n in HALO_NOTES:
        for layer in LAYERS:
            if (n, layer) not in _halo_ref:
                _halo_ref[(n, layer)] = pa.analyse_ref(n, layer, max_dur=3.0)
            x, sr = pa.load(d / f"{n}v{layer}.wav")
            a, b, c = _band_err(_halo_ref[(n, layer)], x, sr)
            num, den, bias = num + a, den + b, bias + c
    return float(np.sqrt(num / den)), float(bias / den)


def main_halo():
    scratch = Path(sys.argv[1])
    p = dict(halo_db=-30.0, halo_key_db=25.0, halo_vel_db=12.0, halo_tau=0.2, halo_fc=1.0,
             halo_fm=0.29, halo_i0=2.5, halo_lp=2.0)
    steps = {k: (4.0 if k.endswith("_db") else 0.35) for k in p}
    steps["halo_fc"] = 0.15
    best, bias = evaluate_halo(p, scratch)
    print("start", round(best, 3), round(bias, 2), flush=True)
    for rnd in range(3):
        for k in steps:
            for sgn in (1, -1):
                q = dict(p)
                q[k] = p[k] + sgn * steps[k] if k.endswith("_db") else p[k] * (1 + sgn * steps[k])
                e, b = evaluate_halo(q, scratch)
                if e < best - 0.02:
                    best, p = e, q
                    print(f"  {k} -> {q[k]:.4g}: {e:.3f} dB (bias {b:+.2f})", flush=True)
                    break
        for k in steps:
            steps[k] *= 0.6
        print("round", rnd, round(best, 3), json.dumps({k: round(v, 4) for k, v in p.items()}), flush=True)


if __name__ == "__main__":
    if len(sys.argv) > 2 and sys.argv[2] == "halo-bands":
        main_halo()
    else:
        main()
