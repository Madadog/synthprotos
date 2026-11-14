"""How smoothly does an engine's sound change from key to key?

Renders every key (A0..C8) at a few velocities with the CLI and measures, per
note, a handful of perceptual features:

  level     loudness of the first 300 ms (A-weighted dB)
  bright    spectral centroid at 50-150 ms, in partials (centroid / f1)
  decay     dB lost between 0.3 s and 2 s
  hf        share of the power above partial 6.5 at 50-150 ms (dB; keys
            where that lies below 12 kHz)

A real piano changes gradually along the keyboard.  The fitted engine is built
from recordings made every third key, so its features have a period-3 ripple
(each recorded key has its own voicing, and the keys in between are blends);
the idealised engine should not.  The roughness of a feature is the rms of its
second difference along the keyboard divided by sqrt(6), which is 0 for any
straight line and equals the ripple amplitude for alternating noise.

usage: python smoothness.py [--presets 0,6] [--layers 4,10,16] [--plots DIR] [--json OUT]
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tempfile
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parents[2]
CLI = ROOT / "target" / "release" / "fmpiano"
FEATURES = ["level", "bright", "decay", "hf"]
UNITS = {"level": "dB", "bright": "partials", "decay": "dB", "hf": "dB"}


def read_wav(path: Path) -> tuple[np.ndarray, int]:
    raw = path.read_bytes()
    sr = int.from_bytes(raw[24:28], "little")
    nch = int.from_bytes(raw[22:24], "little")
    data = np.frombuffer(raw[44:], dtype="<f4")
    return data.reshape(-1, nch).mean(axis=1), sr


def a_weight_db(f):
    f2 = np.asarray(f, float) ** 2
    ra = (12194.0**2 * f2 * f2) / ((f2 + 20.6**2) * np.sqrt((f2 + 107.7**2) * (f2 + 737.9**2)) * (f2 + 12194.0**2))
    return 20 * np.log10(ra + 1e-30) + 2.0


def spectrum(x, sr, t0, t1):
    seg = x[int(t0 * sr): int(t1 * sr)]
    w = np.hanning(len(seg))
    nfft = 1 << int(np.ceil(np.log2(len(seg) * 4)))
    P = np.abs(np.fft.rfft(seg * w, nfft)) ** 2
    f = np.fft.rfftfreq(nfft, 1 / sr)
    return f, P


def features(x, sr, key):
    f1 = 440.0 * 2 ** ((key - 69) / 12)
    onset = int(np.argmax(np.abs(x) > 1e-4 * np.abs(x).max()))
    x = x[onset:]
    f, P = spectrum(x, sr, 0.0, 0.3)
    level = 10 * np.log10((P * 10 ** (a_weight_db(f) / 10)).sum() + 1e-30)
    f, P = spectrum(x, sr, 0.05, 0.15)
    keep = f > 0.5 * f1
    bright = (f[keep] * P[keep]).sum() / P[keep].sum() / f1
    # undefined where partial 6.5 lies above the recordings' band (~12 kHz)
    hf = 10 * np.log10(P[f > 6.5 * f1].sum() / P[keep].sum() + 1e-12) if 6.5 * f1 < 12000 else np.nan

    def rms_db(a, b):
        s = x[int(a * sr): int(b * sr)]
        return 10 * np.log10(np.mean(s * s) + 1e-30)

    decay = rms_db(0.25, 0.35) - rms_db(1.9, 2.1)
    return dict(level=level, bright=bright, decay=decay, hf=hf)


def layer_velocity(layer):
    return 8 + (layer - 1) * (127 - 8) / 15


def sweep(preset, layer, tmp: Path):
    rows = []
    for key in range(21, 109):
        out = tmp / f"p{preset}_k{key}_l{layer}.wav"
        subprocess.run([str(CLI), "note", "--key", str(key), "--layer", str(layer), "--dur", "2.3",
                        "--preset", str(preset), "--mono", "-o", str(out)], check=True)
        x, sr = read_wav(out)
        out.unlink()
        rows.append(features(x, sr, key))
    return {k: np.array([r[k] for r in rows]) for k in FEATURES}


def roughness(y):
    d2 = y[2:] - 2 * y[1:-1] + y[:-2]
    d2 = d2[np.isfinite(d2)]
    return float(np.sqrt(np.mean(d2 * d2) / 6))


def period3(y):
    """Amplitude of the period-3 component of the detrended feature."""
    y = y[np.isfinite(y)]
    k = np.arange(len(y))
    trend = np.polyval(np.polyfit(k, y, 6), k)
    r = y - trend
    z = np.abs(np.sum(r * np.exp(-2j * np.pi * k / 3))) * 2 / len(y)
    return float(z)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--presets", default="0,6")
    ap.add_argument("--layers", default="4,10,16")
    ap.add_argument("--plots", type=Path)
    ap.add_argument("--json", type=Path)
    a = ap.parse_args()
    presets = [int(p) for p in a.presets.split(",")]
    layers = [int(v) for v in a.layers.split(",")]
    names = subprocess.run([str(CLI), "presets"], capture_output=True, text=True).stdout.splitlines()
    res = {}
    with tempfile.TemporaryDirectory() as td:
        for p in presets:
            for layer in layers:
                res[(p, layer)] = sweep(p, layer, Path(td))
    summary = {}
    print(f"{'preset':24s} {'layer':>5s}  " + "  ".join(f"{k:>16s}" for k in FEATURES))
    print(f"{'':24s} {'':5s}  " + "  ".join(f"{'rough / per3':>16s}" for _ in FEATURES))
    for (p, layer), fe in res.items():
        name = names[p] if p < len(names) else str(p)
        cells = []
        for k in FEATURES:
            cells.append(f"{roughness(fe[k]):7.2f} /{period3(fe[k]):6.2f}")
            summary.setdefault(name, {}).setdefault(str(layer), {})[k] = dict(
                rough=roughness(fe[k]), period3=period3(fe[k]), values=fe[k].tolist())
        print(f"{name:24s} {layer:5d}  " + "  ".join(f"{c:>16s}" for c in cells))
    if a.json:
        a.json.write_text(json.dumps(summary))
    if a.plots:
        import matplotlib

        matplotlib.use("Agg")
        import matplotlib.pyplot as plt

        a.plots.mkdir(parents=True, exist_ok=True)
        keys = np.arange(21, 109)
        recorded = [21] + [12 * (o + 1) + k for o in range(1, 8) for k in (0, 3, 6, 9)] + [108]
        fig, axes = plt.subplots(len(FEATURES), 1, figsize=(10, 2.3 * len(FEATURES)), sharex=True)
        cols = ["#c0392b", "#2471a3", "#1e8449", "#7d3c98"]
        for ax, k in zip(axes, FEATURES):
            for i, p in enumerate(presets):
                for j, layer in enumerate(layers):
                    y = res[(p, layer)][k]
                    lab = f"{names[p] if p < len(names) else p}, layer {layer}" if k == FEATURES[0] else None
                    ax.plot(keys, y, "-", color=cols[i % len(cols)], lw=1.0, alpha=0.45 + 0.55 * j / max(1, len(layers) - 1),
                            label=lab)
            for r in recorded:
                ax.axvline(r, color="0.85", lw=0.5, zorder=0)
            ax.set_ylabel(f"{k} ({UNITS[k]})", fontsize=8)
            ax.tick_params(labelsize=7)
        axes[0].legend(fontsize=6, ncol=2)
        axes[-1].set_xlabel("MIDI key (grey lines: recorded keys)", fontsize=8)
        fig.tight_layout()
        fig.savefig(a.plots / "smoothness.png", dpi=80)
        plt.close(fig)


if __name__ == "__main__":
    sys.exit(main())
