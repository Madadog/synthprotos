"""Prototype of the idealised FM piano: physical target -> FM stacks.

`target()` evaluates the smooth laws calibrated by calibrate.py for any key
and velocity; `to_stacks()` turns that partial description into FM stacks
(the same 2-operator stacks the fitted engine uses) by

  1. placing stacks on partials 1-7 and then about 1.22x apart, plus extra
     stacks wherever no sideband reaches a partial in tune,
  2. giving every partial to its nearest stack ("territory"),
  3. moment matching: stack power = territory power, FM index from the
     spread of the territory (an FM spectrum's variance in partial numbers
     is I^2/2),
  4. correcting the overlap between neighbouring stacks (multiplicative
     updates on the stack powers so that each territory gets its power),
  5. envelopes: aftersound decay of the centre partial, prompt sound faster
     by the calibrated ratio; index decaying so the spread follows the
     faster decay of the outer partials.

The Rust engine (crates/fmpiano/src/ideal.rs) implements the same steps,
and starts its stacks at golden-angle phases so that overlapping sidebands
add in power, as assumed here.  `python model.py` prints the mapping error
(laws vs stacks, ERB bands and partials) for a range of keys.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import numpy as np
from scipy.special import jv

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "fit"))
import fmfit as F  # noqa: E402

sys.path.insert(0, str(Path(__file__).resolve().parent))

# smooth inharmonicity / stretch (fits to the measured values, u = (key-64)/44)
LOGB_POLY = [0.45909, -0.87932, 0.32053, 1.77913, -3.40977]
CENTS_POLY = [24.3399, 11.5195, 5.8238, -0.444]


def u_of(m):
    return (m - 64.0) / 44.0


def calib(path=None):
    import calibrate as C

    path = path or ROOT / "tools/ideal/calibration.json"
    p = json.loads(Path(path).read_text())["params"]
    return np.array([p[name] for name in C.NAMES])


def target(th, m, v, *, sr=48000.0, nmax=120):
    """Partial frequencies, initial amplitudes and envelope of a key (the
    strike-point comb is part of the calibrated amplitudes)."""
    import calibrate as C

    u = u_of(m)
    B = 10 ** np.polyval(LOGB_POLY, u)
    cents = np.polyval(CENTS_POLY, u)
    f1 = 440.0 * 2 ** ((m - 69) / 12 + cents / 1200)
    f0 = f1 / np.sqrt(1 + B)
    n = np.arange(1, nmax + 1)
    f = n * f0 * np.sqrt(1 + B * n * n)
    keep = f < min(0.45 * sr, 16000)
    n, f = n[keep], f[keep]
    A = C.amplitudes(th, m, v, n, f)
    sig, w2, r = C.envelope_params(th, m, f)
    return dict(m=m, v=v, f0=f0, B=B, n=n, f=f, A=A, sig=sig, w2=float(w2), ratio=float(r))


# stack centres of the idealised engine (crates/fmpiano/src/ideal.rs)
BASE_CENTERS = [1, 2, 3, 4, 5, 6, 7, 9, 11, 14, 16, 20, 25, 30, 37, 45, 54, 66, 81, 99]


def layout(f0, B, nmax, max_stacks=24):
    """Base centres up to nmax, plus stacks wherever no sideband reaches a
    partial in tune (same rule as ideal.rs)."""
    cs = [c for c in BASE_CENTERS if c <= nmax][:max_stacks]
    covered = set()
    for c in cs:
        jm = F.jmax_for(c, f0, B)
        covered.update(range(max(1, c - jm), c + jm + 1))
    while len(cs) < max_stacks:
        miss = [n for n in range(1, nmax + 1) if n not in covered]
        if not miss:
            break
        first = miss[0]
        nxt = min([c for c in cs if c > first], default=nmax + 1)
        best = first
        for c in range(first, nxt):
            if c - min(F.jmax_for(c, f0, B), c) <= first:
                best = c
            else:
                break
        cs.append(best)
        jm = F.jmax_for(best, f0, B)
        covered.update(range(max(1, best - jm), best + jm + 1))
    return sorted(cs)


def stack_amp(c, n, I):
    """Amplitude on partials n of a unit stack centred on c with index I."""
    return F.stack_spectrum(c, n, np.array([I]))[:, 0]


def to_stacks(tg, max_stacks=24):
    n, A, sig = tg["n"], tg["A"], tg["sig"]
    f0, B = tg["f0"], tg["B"]
    P = A * A
    alive = P > P.max() * 10 ** (-60 / 10)
    nmax = int(n[alive].max())
    cs = layout(f0, B, nmax, max_stacks=max_stacks)
    caps = [F.index_cap(c, f0, B, nmax) for c in cs]
    cs_arr = np.array(cs)
    # territories: nearest centre
    own = np.argmin(np.abs(n[:, None] - cs_arr[None, :]), axis=1)
    own[n > nmax] = -1
    K = len(cs)
    E = np.array([P[own == k].sum() for k in range(K)])
    I = np.zeros(K)
    for k, c in enumerate(cs):
        sel = own == k
        if E[k] <= 0 or sel.sum() == 0:
            continue
        if c == 1:
            # 1:1 stack: match the ratio of partial 2 to partial 1 when it owns both
            if (n[sel] == 2).any():
                r = A[n == 2][0] / max(A[n == 1][0], 1e-30)
                grid = np.linspace(0, caps[k], 200)
                vals = [abs(stack_amp(1, np.array([2]), g)[0]) / max(abs(stack_amp(1, np.array([1]), g)[0]), 1e-9) for g in grid]
                I[k] = grid[int(np.argmin(np.abs(np.array(vals) - r)))]
            continue
        var = (((n[sel] - c) ** 2) * P[sel]).sum() / E[k]
        I[k] = min(np.sqrt(2 * var), caps[k])
    # overlap correction: M[k, l] = share of stack l's power falling in territory k
    M = np.zeros((K, K))
    for l, c in enumerate(cs):
        s2 = stack_amp(c, n, I[l]) ** 2
        for k in range(K):
            M[k, l] = s2[own == k].sum()
    p = E.copy()
    for _ in range(40):
        got = M @ p
        p *= np.where(got > 0, E / np.maximum(got, 1e-300), 1.0)
    amp = np.sqrt(p)
    # envelopes
    stacks = []
    for k, c in enumerate(cs):
        sel = own == k
        sc = sig[n == c][0] if (n == c).any() else sig[sel].mean()
        tau2 = 1.0 / sc
        tau1 = tau2 / tg["ratio"]
        w2 = tg["w2"]
        I0 = I[k]
        if I0 > 0.05 and sel.sum() > 1:
            t1 = tau2
            Pt = P[sel] * np.exp(-2 * sig[sel] * t1)
            var_t = (((n[sel] - c) ** 2) * Pt).sum() / max(Pt.sum(), 1e-300)
            It = min(np.sqrt(2 * var_t), I0 * 0.999)
            tauI = t1 / np.log(I0 / max(It, 1e-3)) if It > 1e-3 else t1 / 7
        else:
            tauI = 1.0
        stacks.append(dict(c=c, A1=amp[k] * (1 - w2), A2=amp[k] * w2, tau1=tau1, tau2=tau2,
                           I0=I0, Iinf=0.0, tauI=tauI))
    return stacks


def stacks_to_x(stacks):
    x = []
    for s in stacks:
        x += [np.log(max(s["A1"], 1e-12)), np.log(max(s["A2"], 1e-12)), np.log(s["tau1"]), np.log(s["tau2"]),
              s["I0"], s["Iinf"], np.log(s["tauI"])]
    return np.array(x), [s["c"] for s in stacks]


def target_power(tg, t):
    """Target partial power over time (prompt + aftersound, coherent)."""
    n, A, sig = tg["n"], tg["A"], tg["sig"]
    w2, r = tg["w2"], tg["ratio"]
    a = A[:, None] * ((1 - w2) * np.exp(-sig[:, None] * r * t[None, :]) + w2 * np.exp(-sig[:, None] * t[None, :]))
    return a * a


def evaluate(cal, m, v, max_stacks=24, **kw):
    tg = target(cal, m, v, **kw)
    st = to_stacks(tg, max_stacks=max_stacks)
    x, cs = stacks_to_x(st)
    t = np.array([0.0, 0.05, 0.2, 0.5, 1.0, 2.0, 4.0])
    Pm = F.model_power(x, cs, tg["n"], t)
    Pt = target_power(tg, t)
    G = F.erb_matrix(tg["f"])
    Lm, Lt = 10 * np.log10(G @ Pm + 1e-30), 10 * np.log10(G @ Pt + 1e-30)
    pk = Lt.max()
    # differences 70 dB below the peak are inaudible (same floor as compare.py)
    Lm, Lt = np.maximum(Lm, pk - 70), np.maximum(Lt, pk - 70)
    w = np.clip((Lt - (pk - 65)) / 45, 0.02, 1.0)
    err = float(np.sqrt(((Lm - Lt) ** 2 * w * w).sum() / (w * w).sum()))
    Lmp, Ltp = 10 * np.log10(Pm + 1e-30), 10 * np.log10(Pt + 1e-30)
    pkp = Ltp.max()
    Lmp, Ltp = np.maximum(Lmp, pkp - 70), np.maximum(Ltp, pkp - 70)
    wp = np.clip((Ltp - (pkp - 65)) / 45, 0.02, 1.0)
    errp = float(np.sqrt(((Lmp - Ltp) ** 2 * wp * wp).sum() / (wp * wp).sum()))
    return err, errp, len(cs)


if __name__ == "__main__":
    cal = calib(sys.argv[1] if len(sys.argv) > 1 else None)
    tot = []
    for m in range(21, 109, 4):
        for v in (0.2, 0.5, 1.0):
            e, ep, k = evaluate(cal, m, v)
            tot.append((e, ep))
            print(f"key {m:3d} v {v:.1f}: band err {e:5.2f} dB  partial err {ep:5.2f} dB  ({k} stacks)")
    tot = np.array(tot)
    print(f"mean band {tot[:, 0].mean():.2f} dB, partial {tot[:, 1].mean():.2f} dB")
