"""Unison-string analysis: how many strings does each recorded key have, and
how far apart are they tuned?

Each partial of a piano note is the sum of a few closely spaced, damped
components: the normal modes of the strings of that note coupled at the
bridge, plus the two polarisations of each string.  A single string gives a
practically single-frequency partial (polarisation splitting is tiny);
bichords and trichords give components a few tenths of a cent to a few cents
apart, which is what makes the partial envelopes beat.

For every partial the signal is isolated in the frequency domain, shifted to
baseband, decimated, and decomposed with ESPRIT (a high-resolution subspace
method that resolves components far closer than an FFT of the same length).

usage: python unison.py [--layer 12] [--json out.json] [--synth RENDER_DIR]
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pianoan as pa  # noqa: E402


def baseband(x, sr, f, bw, t0, t1):
    """Complex baseband signal of the band f +- bw over [t0, t1] (s)."""
    seg = x[int(t0 * sr) : int(t1 * sr)]
    n = len(seg)
    X = np.fft.fft(seg)
    fr = np.fft.fftfreq(n, 1.0 / sr)
    d = np.abs(fr - f) / bw
    mask = np.where(d < 0.6, 1.0, np.where(d < 1.0, 0.5 + 0.5 * np.cos(np.pi * (d - 0.6) / 0.4), 0.0))
    y = np.fft.ifft(X * mask) * np.exp(-2j * np.pi * f * np.arange(n) / sr)
    dec = max(1, int(sr / (4.0 * bw)))
    # drop the filter's circular edge effects
    y = y[: len(y) - int(0.15 * sr)]
    y = y[int(0.05 * sr) :]
    return y[::dec], sr / dec


def esprit(y, K):
    N = len(y)
    L = N // 2
    H = np.array([y[i : i + L] for i in range(N - L + 1)])
    U, s, _ = np.linalg.svd(H, full_matrices=False)
    Us = U[:, :K]
    Phi = np.linalg.lstsq(Us[:-1], Us[1:], rcond=None)[0]
    z = np.linalg.eigvals(Phi)
    V = np.vander(z, N, increasing=True).T
    a = np.linalg.lstsq(V, y, rcond=None)[0]
    return z, a, s


def partial_components(x, sr, f, bw, t0, t1, max_k=6):
    y, fs = baseband(x, sr, f, bw, t0, t1)
    if len(y) < 40:
        return []
    _, _, s = esprit(y, 1)
    # model order: singular values more than 40 dB above the noise tail
    noise = np.median(s[len(s) // 2 :])
    K = int(np.clip(np.sum(s > noise * 10 ** (12 / 20)), 1, max_k))
    z, a, _ = esprit(y, K)
    out = []
    for zk, ak in zip(z, a):
        r = abs(zk)
        if r <= 0 or r >= 1.0:  # growing => noise artefact
            continue
        tau = -1.0 / (np.log(r) * fs)
        fo = np.angle(zk) * fs / (2 * np.pi)
        if abs(fo) > 0.8 * bw or tau < 0.08:
            continue
        out.append(dict(df=float(fo), tau=float(tau), amp=float(abs(ak))))
    if not out:
        return []
    amax = max(c["amp"] for c in out)
    return [c for c in out if c["amp"] > amax * 10 ** (-24 / 20)]


def analyse_note(note, layer, synth_dir=None):
    path = pa.ref_path(note, layer) if synth_dir is None else Path(synth_dir) / f"{note}v{layer}.wav"
    x, sr = pa.load(path)
    on = pa.find_onset(x, sr)
    x = x[on:]
    pitch = pa.estimate_f0_B(x, sr, pa.note_to_midi(note), 0)
    f0, B = pitch.f0, pitch.B
    tr = pa.analyse_tracks(x, sr, f0, B, 0, max_dur=12.0)
    lv = pa.db(tr.amp)
    peak = lv.max()
    bw = min(0.35 * f0, 8.0)
    res = []
    for k in range(min(8, len(tr.partial_n))):
        n = int(tr.partial_n[k])
        fn = float(tr.partial_f[k])
        if lv[k].max() < peak - 30:
            continue
        # analyse while the partial is within 45 dB of its own maximum
        alive = np.nonzero(lv[k] > lv[k].max() - 45)[0]
        t1 = min(tr.t[alive[-1]] if len(alive) else 1.0, 10.0)
        t0 = 0.15
        if t1 - t0 < 0.8:
            continue
        comps = partial_components(x, sr, fn, bw, t0, t1)
        if not comps:
            continue
        fs_ = [c["df"] for c in comps]
        spread_hz = max(fs_) - min(fs_)
        res.append(dict(n=n, f=fn, ncomp=len(comps), spread_hz=spread_hz,
                        spread_cents=1200 * np.log2((fn + spread_hz) / fn), comps=comps))
    return dict(note=note, midi=pa.note_to_midi(note), f0=f0, partials=res)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--layer", type=int, default=12)
    ap.add_argument("--notes", default="all")
    ap.add_argument("--json", type=Path)
    ap.add_argument("--synth", type=Path, help="analyse engine renders (fmpiano refset) instead")
    a = ap.parse_args()
    notes = pa.SALAMANDER_NOTES if a.notes == "all" else a.notes.split(",")
    out = []
    print("key   partials  components(median)  spread cents (median, per partial)")
    for note in notes:
        r = analyse_note(note, a.layer, a.synth)
        out.append(r)
        ps = r["partials"]
        if not ps:
            print(f"{note:4s}  -")
            continue
        nc = np.median([p["ncomp"] for p in ps])
        sp = np.median([p["spread_cents"] for p in ps])
        detail = " ".join(f"{p['n']}:{p['ncomp']}/{p['spread_cents']:.2f}" for p in ps)
        print(f"{note:4s}  {len(ps):2d}        {nc:3.1f}               {sp:5.2f}   [{detail}]", flush=True)
    if a.json:
        a.json.write_text(json.dumps(out, indent=1))


if __name__ == "__main__":
    main()
