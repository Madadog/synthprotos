"""Piano note analysis used to compare the FM model against recorded pianos.

Measures, for a single isolated note:
  * onset time
  * fundamental f0 and string inharmonicity B  (f_n = n f0 sqrt(1 + B n^2))
  * amplitude envelope of every partial (STFT peak tracking at the predicted
    stretched partial frequencies)
  * broadband RMS envelope, spectral centroid and ERB-band energies

Everything works on mono float64 arrays so the same code is applied to the
reference recordings and to renders of the synthesiser.
"""

from __future__ import annotations

import os
from dataclasses import dataclass, field
from pathlib import Path

import numpy as np
import soundfile as sf

NOTE_NAMES = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"]

# The Salamander Grand Piano V3 set (Yamaha C5, CC-BY 3.0, Alexander Holm) is
# sampled every minor third with 16 velocity layers.
SALAMANDER_NOTES = (
    ["A0"]
    + [f"{n}{o}" for o in range(1, 8) for n in ("C", "D#", "F#", "A")]
    + ["C8"]
)

REPO = Path(__file__).resolve().parents[2]
SAMPLE_DIR = Path(os.environ.get("PIANO_SAMPLES", REPO / "samples"))


def note_to_midi(name: str) -> int:
    letter, octave = name[:-1], int(name[-1])
    return NOTE_NAMES.index(letter) + 12 * (octave + 1)


def midi_to_name(m: int) -> str:
    return f"{NOTE_NAMES[m % 12]}{m // 12 - 1}"


def et_freq(m: float) -> float:
    return 440.0 * 2.0 ** ((m - 69) / 12.0)


def ref_path(note: str, vel: int) -> Path:
    return SAMPLE_DIR / f"piano-velocity{vel}" / "audio" / f"{note}v{vel}.ogg"


def load(path, mono: bool = True):
    x, sr = sf.read(str(path), dtype="float64", always_2d=True)
    if mono:
        x = x.mean(axis=1)
    return x, sr


def db(x, floor=1e-12):
    return 20.0 * np.log10(np.maximum(np.abs(x), floor))


def find_onset(x: np.ndarray, sr: int) -> int:
    """First sample where a 1 ms energy envelope reaches -40 dB re. its peak."""
    hop = max(1, sr // 1000)
    n = len(x) // hop
    e = (x[: n * hop].reshape(n, hop) ** 2).mean(axis=1)
    thr = e.max() * 1e-4
    k = int(np.argmax(e > thr))
    # refine to sample resolution inside the frame (and the one before)
    lo = max(0, (k - 1) * hop)
    seg = np.abs(x[lo : (k + 1) * hop])
    i = int(np.argmax(seg > np.sqrt(thr) * 0.5))
    return lo + i


def _spectrum(seg: np.ndarray, sr: int, zp: int = 4):
    w = np.blackman(len(seg))
    nfft = 1 << int(np.ceil(np.log2(len(seg) * zp)))
    X = np.fft.rfft(seg * w, nfft)
    mag = np.abs(X) * 2.0 / w.sum()
    return np.arange(len(X)) * sr / nfft, mag


def partial_freqs(f0: float, B: float, n) -> np.ndarray:
    n = np.asarray(n, dtype=float)
    return n * f0 * np.sqrt(1.0 + B * n * n)


@dataclass
class PitchFit:
    f0: float
    B: float
    cents: float  # deviation of partial 1 from 12-TET (A4 = 440 Hz)
    n_used: int
    partials: list = field(default_factory=list)  # (n, freq, dB)


def _peak_near(lm, df, fk, halfwidth):
    """Parabolic-interpolated local maximum of lm within fk +- halfwidth."""
    lo = int(max(1, (fk - halfwidth) / df))
    hi = int(min(len(lm) - 2, (fk + halfwidth) / df))
    if hi - lo < 2:
        return None
    i = lo + int(np.argmax(lm[lo : hi + 1]))
    if i <= lo or i >= hi:
        return None  # maximum on the edge -> not a peak of this partial
    a, b, c = lm[i - 1], lm[i], lm[i + 1]
    den = a - 2 * b + c
    p = 0.5 * (a - c) / den if den != 0 else 0.0
    return (i + p) * df, b - 0.25 * (a - c) * p


def estimate_f0_B(x: np.ndarray, sr: int, midi: int, onset: int, init: tuple | None = None) -> PitchFit:
    """Estimate f0 and inharmonicity B.

    Coarse grid search on the lowest partials followed by a progressive
    refinement that adds higher partials and refits (f0, B) by weighted least
    squares on (f_n / n)^2 = f0^2 + f0^2 B n^2.

    init=(f0, B) skips the search and only refines from that starting point
    (used to measure a synthesised note against its reference).
    """
    f_nom = et_freq(midi)
    dur = float(np.clip(30.0 / f_nom, 0.3, 1.5))
    start = onset + int(0.05 * sr)
    seg = x[start : start + int(dur * sr)]
    freqs, mag = _spectrum(seg, sr)
    lm = db(mag)
    df = freqs[1]
    fmax = min(0.45 * sr, max(8000.0, 5.5 * f_nom))
    band = (freqs > 0.5 * f_nom) & (freqs < fmax)
    floor = float(np.median(lm[band]))

    # plausible inharmonicity range for this register (grand pianos)
    b_max = 1.5e-3 if midi < 60 else 4e-2

    # ---- stage 0: "inharmonic sieve" over a (f0, B) grid using spectral peaks
    from scipy.signal import find_peaks

    lim = min(fmax, 30.5 * f_nom)
    pk, _ = find_peaks(lm, prominence=8.0)
    pk = pk[(freqs[pk] > 0.6 * f_nom) & (freqs[pk] < lim) & (lm[pk] > floor + 12.0)]
    fp = freqs[pk]
    wp = np.clip(lm[pk] - floor, 0.0, 50.0)
    f0g = f_nom * 2.0 ** (np.arange(-100.0, 100.01, 0.2) / 1200.0)
    bg = np.concatenate(([0.0], np.geomspace(1e-5, b_max, 260)))
    best = (-1.0, f_nom, 0.0)
    if len(fp):
        r = fp[None, :] / f0g[:, None]  # [nf0, npk]
        for B in bg:
            nu = r / np.sqrt(1.0 + B * r * r)
            nu = r / np.sqrt(1.0 + B * nu * nu)  # one fixed-point refinement
            n_ = np.maximum(np.round(nu), 1.0)
            # deviation (cents) from the nearest predicted partial
            e = 1200.0 * np.log2(r / (n_ * np.sqrt(1.0 + B * n_ * n_)))
            sc = (wp * np.clip(1.0 - (e / 15.0) ** 2, 0.0, None)).sum(axis=1)
            i = int(np.argmax(sc))
            if sc[i] > best[0]:
                best = (float(sc[i]), float(f0g[i]), float(B))
    _, f0, B = best
    if init is not None:
        f0, B = init

    if f_nom > 1800.0 and init is None:
        # top octaves: only 2-4 partials exist, trust the (dominant) fundamental
        pk1 = None
        lo_f, hi_f = f_nom * 2 ** (-1.5 / 12), f_nom * 2 ** (1.5 / 12)
        i0, i1 = int(lo_f / df), int(hi_f / df)
        i = i0 + int(np.argmax(lm[i0:i1]))
        pk1 = _peak_near(lm, df, i * df, 3 * df)
        if pk1 is not None:
            f1 = pk1[0]
            found = [(1, f1, pk1[1])]
            for k in (2, 3, 4):
                # B in [0, 0.04] -> f_k/f_1 in [k, k*sqrt((1+0.04k^2)/1.04)]
                hi_r = k * np.sqrt((1 + 0.04 * k * k) / 1.04)
                a, b = int(k * f1 * 0.995 / df), int(hi_r * f1 * 1.005 / df)
                if b >= len(lm) - 2 or k * f1 > 0.45 * sr:
                    break
                j = a + int(np.argmax(lm[a:b]))
                pkk = _peak_near(lm, df, j * df, 3 * df)
                if pkk is not None and pkk[1] > floor + 15.0:
                    found.append((k, pkk[0], pkk[1]))
            if len(found) >= 2:
                kk = np.array([f[0] for f in found], float)
                ff = np.array([f[1] for f in found])
                y = (ff / kk) ** 2
                A = np.stack([np.ones_like(kk), kk * kk], axis=1)
                sol, *_ = np.linalg.lstsq(A, y, rcond=None)
                B = float(np.clip(sol[1] / sol[0], 0.0, b_max)) if sol[0] > 0 else 0.0
            else:
                B = 0.0
            f0 = f1 / np.sqrt(1.0 + B)
            cents = 1200.0 * np.log2(f1 / f_nom)
            return PitchFit(f0=f0, B=B, cents=float(cents), n_used=len(found), partials=found)

    # ---- progressive refinement
    found = []
    for kmax in (6, 10, 16, 24, 36, 50, 70, 100):
        for _ in range(2):
            found = []
            for k in range(1, kmax + 1):
                fk = float(partial_freqs(f0, B, k))
                if fk >= fmax:
                    break
                spacing = f0 * (1.0 + 1.5 * B * k * k)
                hw = max(0.25 * spacing if k > 6 else min(0.25 * spacing, 0.015 * fk), 2.5 * df)
                pk = _peak_near(lm, df, fk, hw)
                if pk is not None and pk[1] > floor + 12.0:
                    found.append((k, pk[0], pk[1]))
            if len(found) >= 3 and found[-1][0] - found[0][0] >= 2:
                kk = np.array([f[0] for f in found], float)
                ff = np.array([f[1] for f in found])
                ww = np.clip(np.array([f[2] for f in found]) - floor, 1.0, 60.0)
                keep = np.ones(len(kk), bool)
                for _it in range(3):  # robust LS with outlier rejection
                    y = (ff[keep] / kk[keep]) ** 2
                    A = np.stack([np.ones(keep.sum()), kk[keep] ** 2], axis=1)
                    w_ = ww[keep]
                    sol, *_ = np.linalg.lstsq(A * w_[:, None], y * w_, rcond=None)
                    if sol[0] <= 0:
                        break
                    f0_ = float(np.sqrt(sol[0]))
                    B_ = float(np.clip(sol[1] / sol[0], 0.0, b_max))
                    res = 1200 * np.log2(ff / partial_freqs(f0_, B_, kk))
                    mad = np.median(np.abs(res[keep])) + 1e-9
                    new = np.abs(res) < max(4.0, 4.0 * mad)
                    if new.sum() < 3 or (new == keep).all():
                        break
                    keep = new
                if sol[0] > 0:
                    f0, B = f0_, B_
                    found = [f for f, k in zip(found, keep) if k]
            elif found:
                # too few partials for B: keep B, fit f0 to the strongest one
                k, fpk, _ = max(found, key=lambda f: f[2])
                f0 = fpk / (k * np.sqrt(1.0 + B * k * k))
        if partial_freqs(f0, B, kmax) >= fmax:
            break
    p1 = partial_freqs(f0, B, 1)
    cents = 1200.0 * np.log2(p1 / f_nom)
    return PitchFit(f0=f0, B=B, cents=float(cents), n_used=len(found), partials=found)


# ---------------------------------------------------------------- ERB bands
def erb_edges(fmin=30.0, fmax=16000.0, step=1.0):
    def hz2erb(f):
        return 21.4 * np.log10(1 + 0.00437 * f)

    def erb2hz(e):
        return (10 ** (e / 21.4) - 1) / 0.00437

    e = np.arange(hz2erb(fmin), hz2erb(fmax) + step, step)
    return erb2hz(e)


@dataclass
class Tracks:
    sr: int
    f0: float
    B: float
    t: np.ndarray  # frame centre times (s) relative to onset
    partial_n: np.ndarray  # partial numbers
    partial_f: np.ndarray  # partial frequencies
    amp: np.ndarray  # [n_partials, n_frames] linear amplitude
    rms_db: np.ndarray  # [n_frames]
    centroid: np.ndarray  # [n_frames] Hz
    erb_f: np.ndarray  # band centres
    erb_db: np.ndarray  # [n_frames, n_bands] (power dB)


def analyse_tracks(
    x: np.ndarray,
    sr: int,
    f0: float,
    B: float,
    onset: int,
    hop_s: float = 0.005,
    max_dur: float | None = None,
    fmax: float = 16000.0,
) -> Tracks:
    win_s = float(np.clip(6.0 / f0, 0.016, 0.25))
    W = int(win_s * sr) // 2 * 2
    hop = int(hop_s * sr)
    w = np.hanning(W)
    nfft = 1 << int(np.ceil(np.log2(W * 2)))
    wsum = w.sum()
    end = len(x) if max_dur is None else min(len(x), onset + int(max_dur * sr))
    xp = np.concatenate([np.zeros(W), x[:end], np.zeros(W)])
    # frame k is centred at onset + k*hop  (index shifted by W because of padding)
    nfr = (end - onset) // hop
    centres = onset + np.arange(nfr) * hop
    df = sr / nfft
    freqs = np.arange(nfft // 2 + 1) * df

    nmax = int(min(120, fmax / f0))
    pn = np.arange(1, nmax + 1)
    pf = partial_freqs(f0, B, pn)
    keep = pf < min(fmax, 0.47 * sr)
    pn, pf = pn[keep], pf[keep]
    spacing = f0 * (1.0 + 1.5 * B * pn * pn)
    lo = np.clip(np.floor((pf - 0.3 * spacing) / df).astype(int), 0, len(freqs) - 1)
    hi = np.clip(np.ceil((pf + 0.3 * spacing) / df).astype(int), 0, len(freqs) - 1)

    edges = erb_edges(30.0, min(fmax, 0.47 * sr))
    eb = np.searchsorted(freqs, edges)
    erb_f = np.sqrt(edges[:-1] * edges[1:])

    amp = np.zeros((len(pn), nfr))
    rms = np.zeros(nfr)
    cen = np.zeros(nfr)
    erb = np.zeros((nfr, len(edges) - 1))
    blk = 128
    for b0 in range(0, nfr, blk):
        idx = centres[b0 : b0 + blk]
        frames = np.stack([xp[c + W - W // 2 : c + W + W // 2] for c in idx])
        X = np.fft.rfft(frames * w, nfft, axis=1)
        mag = np.abs(X) * 2.0 / wsum
        pw = mag**2
        for j in range(len(pn)):
            amp[j, b0 : b0 + len(idx)] = mag[:, lo[j] : hi[j] + 1].max(axis=1)
        # mean-square of the signal (Parseval with window normalisation)
        tot = pw.sum(axis=1) * 0.5 * (wsum**2) / (W * (w**2).sum())
        rms[b0 : b0 + len(idx)] = tot
        cen[b0 : b0 + len(idx)] = (pw * freqs).sum(axis=1) / np.maximum(pw.sum(axis=1), 1e-30)
        cs = np.concatenate([np.zeros((len(idx), 1)), np.cumsum(pw, axis=1)], axis=1)
        erb[b0 : b0 + len(idx)] = cs[:, eb[1:]] - cs[:, eb[:-1]]
    t = (centres - onset) / sr
    return Tracks(
        sr=sr,
        f0=f0,
        B=B,
        t=t,
        partial_n=pn,
        partial_f=pf,
        amp=amp,
        rms_db=10 * np.log10(np.maximum(rms, 1e-20)),
        centroid=cen,
        erb_f=erb_f,
        erb_db=10 * np.log10(np.maximum(erb, 1e-20)),
    )


def decay_rate(t, ydb, t0, t1, floor_db=None):
    """Slope (dB/s) of a least-squares line through ydb on [t0, t1]."""
    m = (t >= t0) & (t <= t1)
    if floor_db is not None:
        m &= ydb > floor_db
    if m.sum() < 5:
        return float("nan")
    p = np.polyfit(t[m], ydb[m], 1)
    return float(p[0])


@dataclass
class NoteAnalysis:
    name: str
    midi: int
    vel: int
    pitch: PitchFit
    tracks: Tracks
    onset: int
    peak_db: float


def analyse_signal(x, sr, midi, name="", vel=0, pitch: PitchFit | None = None, max_dur=None):
    onset = find_onset(x, sr)
    if pitch is None:
        pitch = estimate_f0_B(x, sr, midi, onset)
    tr = analyse_tracks(x, sr, pitch.f0, pitch.B, onset, max_dur=max_dur)
    return NoteAnalysis(
        name=name,
        midi=midi,
        vel=vel,
        pitch=pitch,
        tracks=tr,
        onset=onset,
        peak_db=float(tr.rms_db.max()),
    )


def analyse_ref(note: str, vel: int, max_dur=None) -> NoteAnalysis:
    x, sr = load(ref_path(note, vel))
    return analyse_signal(x, sr, note_to_midi(note), note, vel, max_dur=max_dur)
