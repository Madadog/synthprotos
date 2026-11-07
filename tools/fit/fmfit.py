"""Analytic fitting of the banded-FM piano model to measured partial envelopes.

The voice is a set of 2-operator FM "stacks".  Stack k has its carrier on the
(stretched) partial c_k and its modulator at the local partial spacing, so its
sidebands land on the neighbouring inharmonic partials.  For c_k == 1 the
modulator ratio is exactly 1:1 and the negative sidebands fold back onto the
harmonics (classic 1:1 FM spectrum).

Because an FM stack has an exact Bessel-function spectrum, the power that the
model puts on every partial at time t is known in closed form.  That lets us
fit envelopes to the reference recordings without rendering audio:

    a_k(t) = A1_k e^{-t/tau1_k} + A2_k e^{-t/tau2_k}
    I_k(t) = Iinf_k + (I0_k - Iinf_k) e^{-t/tauI_k}
    P_n(t) = sum_k abar_k(t)^2 S_k(n, I_k(t))^2

where abar is a_k averaged over the analysis window (what an STFT frame of
the recording actually measures), so the attack frames can be fitted too.

Stack centres are chosen per key from the measured inharmonicity: a stack
only "owns" the sidebands that land within a few Hz of a real partial
(beating no faster than unison strings), and its index is capped so that
the mistuned sidebands beyond that stay ~20 dB down.
"""

from __future__ import annotations

import sys
from dataclasses import dataclass
from pathlib import Path

import numpy as np
from scipy.optimize import least_squares
from scipy.special import jv

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "analysis"))
import pianoan as pa  # noqa: E402

NOISE = np.loadtxt(Path(pa.__file__).with_name("noise_floor.txt"))


def noise_db_for(freqs, win_samples):
    """Expected noise power (dB) of a partial track bin for a given window."""
    L = np.interp(np.log(freqs), np.log(NOISE[:, 0]), NOISE[:, 1])
    return L + 10 * np.log10(8192.0 / win_samples) + 3.0  # +3: max over bins


@dataclass
class Target:
    note: str
    vel: int
    midi: int
    f0: float
    B: float
    n: np.ndarray  # partial numbers
    f: np.ndarray  # partial frequencies
    t: np.ndarray  # fit frame times
    L: np.ndarray  # [n, t] reference level (dB, power of partial amplitude)
    noise: np.ndarray  # [n] noise level dB
    peak: float  # max partial level (dB)
    analysis: object
    win_s: float  # analysis window length (s)


FIT_TIMES = np.array(
    [0.006, 0.01, 0.015, 0.02, 0.028, 0.038, 0.05, 0.065, 0.08, 0.1, 0.13, 0.17, 0.22, 0.3, 0.4,
     0.52, 0.7, 0.9, 1.2, 1.6, 2.1, 2.8, 3.6, 4.7, 6.0, 8.0, 10.5, 14.0, 18.0, 23.0]
)


def make_target(note: str, vel: int, max_partials: int = 90, fmax: float = 15000.0) -> Target:
    a = pa.analyse_ref(note, vel)
    tr = a.tracks
    keep = (tr.partial_n <= max_partials) & (tr.partial_f < fmax)
    amp = tr.amp[keep]
    n = tr.partial_n[keep]
    f = tr.partial_f[keep]
    P = amp**2
    win = int(np.clip(6.0 / tr.f0, 0.016, 0.25) * tr.sr) // 2 * 2
    noise = noise_db_for(f, win)
    # frames: smooth power over [0.85 t, 1.18 t]
    t_end = tr.t[-1] - 0.1
    tf = FIT_TIMES[FIT_TIMES < t_end]
    L = np.zeros((len(n), len(tf)))
    for j, t in enumerate(tf):
        m = (tr.t >= t * 0.85 - 0.003) & (tr.t <= t * 1.18 + 0.003)
        L[:, j] = 10 * np.log10(P[:, m].mean(axis=1) + 1e-30)
    peak = float(L.max())
    # drop frames where everything is in the noise
    alive = (L - noise[:, None] > 6).any(axis=0)
    last = np.max(np.nonzero(alive)[0]) + 1 if alive.any() else len(tf)
    win_s = win / tr.sr
    return Target(note, vel, pa.note_to_midi(note), tr.f0, tr.B, n, f, tf[:last], L[:, :last], noise, peak, a, win_s)


# ------------------------------------------------------------------ model
# Per stack: log A1, log A2, log tau1, log tau2, I0, Iinf, log tauI.
# The carrier is rendered as two slightly detuned copies ("prompt" sound with
# amplitude A1 e^{-t/tau1} and "aftersound" with A2 e^{-t/tau2}).  They start
# in phase (like unison strings struck by one hammer) and the detuning is
# small, so their amplitudes add coherently: a(t) = A1 e1 + A2 e2; the slow
# drift between them then produces the beating heard in real pianos.
NPAR = 7
LN10 = np.log(10.0)


def stack_spectrum(c: int, n: np.ndarray, I: np.ndarray, deriv: bool = False):
    """Amplitude on partial n of a unit-amplitude stack centred on c (n x t).

    With deriv=True also returns d/dI.  dJ_m/dI = (J_{m-1} - J_{m+1}) / 2.
    """
    nn = n[:, None].astype(float)
    Ib = I[None, :]
    if c == 1:
        sgn = (-1.0) ** nn
        S = jv(nn - 1, Ib) + sgn * jv(nn + 1, Ib)
        if not deriv:
            return S
        dS = 0.5 * (jv(nn - 2, Ib) - jv(nn, Ib)) + sgn * 0.5 * (jv(nn, Ib) - jv(nn + 2, Ib))
        return S, dS
    m = nn - c
    S = jv(m, Ib)
    if not deriv:
        return S
    return S, 0.5 * (jv(m - 1, Ib) - jv(m + 1, Ib))


def unpack(x, k):
    p = x[k * NPAR : (k + 1) * NPAR]
    return dict(
        A1=np.exp(p[0]),
        A2=np.exp(p[1]),
        tau1=np.exp(p[2]),
        tau2=np.exp(p[3]),
        I0=p[4],
        Iinf=p[5],
        tauI=np.exp(p[6]),
    )


def stack_env(q, t):
    """Power envelope p(t) and index I(t) of one stack."""
    p = (q["A1"] * np.exp(-t / q["tau1"]) + q["A2"] * np.exp(-t / q["tau2"])) ** 2
    I = q["Iinf"] + (q["I0"] - q["Iinf"]) * np.exp(-t / q["tauI"])
    return p, I


def window_grid(t, win_s, K=24):
    """Sub-sample times and Hann weights for averaging an envelope over the
    analysis window centred on each frame time."""
    u = ((np.arange(K) + 0.5) / K - 0.5) * win_s
    w = np.sin(np.pi * (np.arange(K) + 0.5) / K) ** 2
    w /= w.sum()
    tt = t[:, None] + u[None, :]
    return tt, w


def _wexp(tt, w, tau):
    """Window-average of e^{-t/tau} (zero before the onset) and d/dlog tau."""
    e = np.where(tt >= 0, np.exp(-np.maximum(tt, 0.0) / tau), 0.0)
    g = e @ w
    dg = (e * (np.maximum(tt, 0.0) / tau)) @ w
    return g, dg


def model_power(x, centers, n, t, jac=False, win=None):
    """Partial power P[n, t]; with jac=True also dP/dx as [n, t, len(x)].

    win: (tt, w) from window_grid(); if None the envelope is point-sampled."""
    P = np.zeros((len(n), len(t)))
    dP = np.zeros((len(n), len(t), len(x))) if jac else None
    for k, c in enumerate(centers):
        q = unpack(x, k)
        if win is None:
            g1 = np.exp(-t / q["tau1"])
            g2 = np.exp(-t / q["tau2"])
            dg1 = g1 * (t / q["tau1"])
            dg2 = g2 * (t / q["tau2"])
        else:
            g1, dg1 = _wexp(win[0], win[1], q["tau1"])
            g2, dg2 = _wexp(win[0], win[1], q["tau2"])
        u1 = q["A1"] * g1
        u2 = q["A2"] * g2
        eI = np.exp(-t / q["tauI"])
        a = u1 + u2
        p = a * a
        I = q["Iinf"] + (q["I0"] - q["Iinf"]) * eI
        if not jac:
            S = stack_spectrum(c, n, I)
            P += p[None, :] * S * S
            continue
        S, dS = stack_spectrum(c, n, I, deriv=True)
        S2 = S * S
        P += p[None, :] * S2
        g_idx = 2.0 * p[None, :] * S * dS  # dP/dI
        dp = [2.0 * a * u1, 2.0 * a * u2, 2.0 * a * q["A1"] * dg1, 2.0 * a * q["A2"] * dg2]
        dI = [eI, 1.0 - eI, (q["I0"] - q["Iinf"]) * eI * (t / q["tauI"])]
        o = k * NPAR
        for j in range(4):
            dP[:, :, o + j] = S2 * dp[j][None, :]
        for j in range(3):
            dP[:, :, o + 4 + j] = g_idx * dI[j][None, :]
    return (P, dP) if jac else P


def partial_f(n, f0, B):
    n = np.asarray(n, float)
    return n * f0 * np.sqrt(1.0 + B * n * n)


def sideband_error(c, j, f0, B):
    """Hz between sideband j of a stack on partial c and the real partial c+j."""
    fc = partial_f(c, f0, B)
    fm = fc if c == 1 else 0.5 * (partial_f(c + 1, f0, B) - partial_f(c - 1, f0, B))
    return fc + j * fm - partial_f(c + j, f0, B)


def beat_tolerance(f):
    """Largest acceptable sideband mistuning (Hz).  A mistuned sideband beats
    against the real partial; keep that below ~12 Hz (roughness sets in above)
    and within 1/5 of a critical band."""
    erb = 24.7 * (4.37 * np.asarray(f) / 1000.0 + 1.0)
    return np.clip(0.2 * erb, 2.5, 12.0)


def jmax_for(c, f0, B, jlim=12):
    """Largest |j| for which sidebands c-|j|..c+|j| all land within tolerance."""
    best = 0
    for j in range(1, jlim + 1):
        ok = True
        for sj in (j, -j):
            if c + sj < 1:
                continue  # folds below DC, handled by index caps
            e = abs(sideband_error(c, sj, f0, B))
            if e > beat_tolerance(partial_f(c + sj, f0, B)):
                ok = False
        if not ok:
            break
        best = j
    return best


BASE_CENTERS = (1, 2, 3, 4, 6, 9, 13, 18, 25, 35, 50, 70)


def layout(f0, B, nmax, max_stacks=14):
    """Choose stack centres: a fixed, roughly geometric set (dense where the
    ear resolves single partials), then extra stacks wherever a partial up to
    nmax cannot be reached in tune by an existing stack's sidebands."""
    centers = [c for c in BASE_CENTERS if c <= nmax]
    covered = set()
    for c in centers:
        jm = jmax_for(c, f0, B)
        covered.update(range(max(1, c - jm), c + jm + 1))
    while len(centers) < max_stacks:
        gaps = [k for k in range(1, nmax + 1) if k not in covered]
        if not gaps:
            break
        first = gaps[0]
        nxt = min([c for c in centers if c > first], default=nmax + 1)
        best = first
        for c in range(first, nxt):
            if c - jmax_for(c, f0, B) <= first:
                best = c
            else:
                break
        jm = jmax_for(best, f0, B)
        centers.append(best)
        covered.update(range(max(1, best - jm), best + jm + 1))
    return sorted(set(centers))


def nmax_of(tg: Target):
    alive = (tg.L[:, : max(1, len(tg.t) // 2)] > np.maximum(tg.peak - 70, tg.noise[:, None] + 6)).any(axis=1)
    return int(tg.n[alive].max()) if alive.any() else 1


def default_centers(tg: Target):
    return layout(tg.f0, tg.B, nmax_of(tg))


def index_cap(c, f0=None, B=None, n_top=None):
    """Upper bound of a stack's index.

    * c = 1 (1:1) up to 3; stretched stacks I <= 0.55 c so that no sideband
      reaches DC;
    * sidebands beyond the in-tune range jmax must stay weak (I <= 0.9 jmax);
    * sidebands should not run past the analysed partials (n_top)."""
    cap = 3.0 if c == 1 else max(0.6, 0.55 * c)
    if f0 is not None:
        jm = jmax_for(c, f0, B)
        cap = min(cap, 0.15 if jm == 0 else 0.9 * jm)
    if n_top is not None and c > 1:
        cap = min(cap, max(0.15, 0.8 * (n_top - c)))
    return float(cap)


def bounds_for(centers, caps):
    lo, hi = [], []
    for c, cap in zip(centers, caps):
        lo += [np.log(1e-8), np.log(1e-8), np.log(0.003), np.log(0.2), 0.0, 0.0, np.log(0.005)]
        hi += [np.log(10.0), np.log(10.0), np.log(4.0), np.log(80.0), cap, cap, np.log(40.0)]
    return np.array(lo), np.array(hi)


def init_for(centers, caps, n, Lref):
    init = []
    for c, cap in zip(centers, caps):
        k = int(np.argmin(np.abs(n - c)))
        a0 = 10 ** (Lref[k, :6].max() / 20)
        init += [np.log(0.8 * a0 + 1e-9), np.log(0.3 * a0 + 1e-9), np.log(0.4), np.log(4.0),
                 min(0.6, cap), min(0.2, cap), np.log(0.5)]
    return np.array(init)


def erb_matrix(f, step=1.0):
    """Aggregation matrix [bands, partials]: partial -> ERB band (step ERB wide)."""
    e = 21.4 * np.log10(1 + 0.00437 * np.asarray(f))
    b = np.floor(e / step).astype(int)
    ub = np.unique(b)
    G = (b[None, :] == ub[:, None]).astype(float)
    return G


def fit_setup(tg: Target, erb=True):
    t = tg.t
    Lp = np.maximum(tg.L, tg.noise[:, None])
    if erb:
        G = erb_matrix(tg.f)
    else:
        G = np.eye(len(tg.n))
    floor_pow = G @ (10 ** (tg.noise / 10))
    Lref = 10 * np.log10(G @ (10 ** (Lp / 10)))
    peak = Lref.max()
    # perceptual weight: components > ~60 dB below the peak hardly matter
    w = np.clip((Lref - (peak - 65)) / 45, 0.02, 1.0)
    # frames whose analysis window still overlaps the onset only bound the
    # model from above (no hidden fast-decaying energy), see fit()
    early = t < 0.6 * tg.win_s
    return t, Lref, floor_pow, w, G, window_grid(t, tg.win_s), early


def fit(tg: Target, centers=None, x0=None, verbose=0, restarts=6, seed=0, prior=None, prior_w=0.0, erb=True, max_nfev=400):
    """Fit stack envelopes to a target (loss on ERB-band levels by default).

    prior/prior_w: optional parameter vector that the solution is gently
    pulled towards (used for continuation across keys/velocities)."""
    if centers is None:
        centers = default_centers(tg)
    n = tg.n
    t, Lref, floor_pow, w, G, win, early = fit_setup(tg, erb)
    w = w / np.sqrt(len(t))
    slack = np.where(early, 3.0, 0.0)[None, :]
    caps = [index_cap(c, tg.f0, tg.B, int(n.max())) for c in centers]
    lo, hi = bounds_for(centers, caps)
    base = init_for(centers, caps, n, np.maximum(tg.L, tg.noise[:, None])) if x0 is None else np.asarray(x0, float)
    span = hi - lo

    def resid(x):
        P = G @ model_power(x, centers, n, t, win=win)
        Lm = 10 * np.log10(P + floor_pow[:, None])
        d = Lm - Lref
        d = np.where(early[None, :], np.maximum(d - slack, 0.0), d)
        r = (d * w).ravel()
        if prior is not None and prior_w > 0:
            r = np.concatenate([r, prior_w * (x - prior) / span])
        return r

    def jac(x):
        P, dP = model_power(x, centers, n, t, jac=True, win=win)
        P = G @ P
        dP = np.einsum("bn,ntk->btk", G, dP)
        g = (10.0 / LN10) / (P + floor_pow[:, None]) * w
        if early.any():
            Lm = 10 * np.log10(P + floor_pow[:, None])
            inactive = early[None, :] & (Lm - Lref - slack <= 0.0)
            g = np.where(inactive, 0.0, g)
        J = (dP * g[:, :, None]).reshape(-1, len(x))
        if prior is not None and prior_w > 0:
            J = np.concatenate([J, np.diag(prior_w / span)], axis=0)
        return J

    rng = np.random.default_rng(seed)
    best = None
    for r_ in range(restarts):
        x0_ = base.copy()
        if r_ > 0:
            x0_ = x0_ + rng.normal(0, 1, len(x0_)) * span * 0.15
        x0_ = np.clip(x0_, lo + 1e-9, hi - 1e-9)
        r = least_squares(resid, x0_, jac=jac, bounds=(lo, hi), x_scale="jac", verbose=verbose, max_nfev=max_nfev)
        if best is None or r.cost < best.cost:
            best = r
    x = best.x
    return dict(x=x, centers=list(centers), caps=caps, cost=float(best.cost), **evaluate(tg, x, centers, erb))


def evaluate(tg: Target, x, centers, erb=True):
    """RMS (weighted) dB error of a parameter vector, on bands or partials."""
    t, Lref, floor_pow, w, G, win, early = fit_setup(tg, erb)
    P = G @ model_power(x, centers, tg.n, t, win=win)
    Lm = 10 * np.log10(P + floor_pow[:, None])
    ww = w**2 * (~early)[None, :]  # report the error on fully-resolved frames
    rms = float(np.sqrt((((Lm - Lref) ** 2) * ww).sum() / ww.sum()))
    return dict(rms_db=rms, Lm=Lm, Lref=Lref, t=t)
