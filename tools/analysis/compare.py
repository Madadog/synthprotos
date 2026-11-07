"""Compare renders of the FM engine against the reference recordings.

usage:
  cargo run --release -p fmpiano-cli -- refset --outdir out/refset --layers 4,10,16
  python tools/analysis/compare.py out/refset --layers 4,10,16 [--plots DIR] [--json FILE]

Both signals are analysed with exactly the same code (pianoan.py):
  * ERB-band spectrogram error (dB), time-smoothed, clipped at the room noise
    of the recordings, weighted towards louder bands (same weighting as the fit)
  * levels of partials 1-8 at 0.1 s, 0.5 s and 2 s
  * RMS-envelope decay rates, spectral centroid, pitch of partial 1 and B
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pianoan as pa  # noqa: E402

FRAMES = np.array([0.01, 0.02, 0.035, 0.05, 0.075, 0.1, 0.15, 0.2, 0.3, 0.45, 0.65, 0.9,
                   1.2, 1.6, 2.1, 2.8, 3.6, 4.7, 6.0, 8.0])
NOISE = np.loadtxt(Path(pa.__file__).with_name("noise_floor.txt"))


def smoothed(t, y_db, frames):
    """Power-average a dB track over [0.85 t, 1.18 t] around each frame."""
    p = 10 ** (y_db / 10)
    out = np.full((len(frames),) + y_db.shape[1:], np.nan)
    for j, f in enumerate(frames):
        m = (t >= f * 0.85 - 0.003) & (t <= f * 1.18 + 0.003)
        if m.any():
            out[j] = 10 * np.log10(p[m].mean(axis=0) + 1e-30)
    return out


def a_weight_db(f):
    f = np.asarray(f, float)
    f2 = f * f
    ra = (12194.0**2 * f2 * f2) / ((f2 + 20.6**2) * np.sqrt((f2 + 107.7**2) * (f2 + 737.9**2)) * (f2 + 12194.0**2))
    return 20 * np.log10(ra) + 2.0


def band_noise(erb_f, win):
    L = np.interp(np.log(erb_f), np.log(NOISE[:, 0]), NOISE[:, 1])
    # noise power summed over an ERB band of bins of a window of `win` samples
    return L + 10 * np.log10(8192.0 / win) + 10 * np.log10(np.maximum(1, erb_f * 0.108 * win / 48000))


def compare_one(note, layer, synth_dir: Path, max_dur=8.5):
    midi = pa.note_to_midi(note)
    ra = pa.analyse_ref(note, layer, max_dur=max_dur)
    x, sr = pa.load(synth_dir / f"{note}v{layer}.wav")
    onset = pa.find_onset(x, sr)
    spitch = pa.estimate_f0_B(x, sr, midi, onset, init=(ra.pitch.f0, ra.pitch.B))
    sa = pa.analyse_signal(x, sr, midi, note, layer, pitch=spitch, max_dur=max_dur)
    # also analyse the synth with the *reference* f0/B so partial tracks line up
    sa_ref = pa.analyse_signal(x, sr, midi, note, layer, pitch=ra.pitch, max_dur=max_dur)

    rt, st = ra.tracks, sa_ref.tracks
    nb = min(rt.erb_db.shape[1], st.erb_db.shape[1])
    nb = min(nb, int(np.searchsorted(rt.erb_f, 15000.0)))  # recordings are band-limited (codec)
    frames = FRAMES[FRAMES < min(rt.t[-1], st.t[-1]) - 0.1]
    R = smoothed(rt.t, rt.erb_db[:, :nb], frames)
    S = smoothed(st.t, st.erb_db[:, :nb], frames)
    win = int(np.clip(6.0 / rt.f0, 0.016, 0.25) * sr) // 2 * 2
    fl = band_noise(rt.erb_f[:nb], win)[None, :]
    # differences below the room noise, or 70 dB below the loudest band
    # (inaudible), are not counted
    fl = np.maximum(fl, R.max() - 70.0)
    Rc, Sc = np.maximum(R, fl), np.maximum(S, fl)
    # weights follow A-weighted level re. the loudest band: components far
    # below the peak, or in the deep bass the ear barely hears, count little
    RA = Rc + a_weight_db(rt.erb_f[:nb])[None, :]
    peak = RA.max()
    alive = (Rc > fl + 3) | (Sc > fl + 3)
    w = np.clip((RA - (peak - 65)) / 45, 0.02, 1.0) * alive
    band_err = float(np.sqrt((((Sc - Rc) ** 2) * w**2).sum() / max((w**2).sum(), 1e-9)))
    band_bias = float(((Sc - Rc) * w**2).sum() / max((w**2).sum(), 1e-9))

    def plevels(tr, ts):
        out = []
        for tt in ts:
            k = int(np.argmin(np.abs(tr.t - tt)))
            lo, hi = max(0, k - 3), k + 4
            out.append(10 * np.log10((tr.amp[:8, lo:hi] ** 2).mean(axis=1) + 1e-30))
        return np.array(out)

    ts = [0.1, 0.5, 2.0]
    pr, ps = plevels(rt, ts), plevels(st, ts)
    rel_r = pr - pr.max()
    ok = rel_r > -45
    perr = float(np.sqrt(np.mean((ps - pr)[ok] ** 2)))

    def slope(tr, a, b):
        return pa.decay_rate(tr.t, tr.rms_db, a, b)

    def cen(tr, tt):
        k = int(np.argmin(np.abs(tr.t - tt)))
        return float(np.median(tr.centroid[max(0, k - 5) : k + 6]))

    res = dict(
        note=note,
        layer=layer,
        band_err_db=band_err,
        band_bias_db=band_bias,
        partial_err_db=perr,
        peak_ref_db=float(rt.rms_db.max()),
        peak_syn_db=float(st.rms_db.max()),
        decay_early_ref=slope(rt, 0.1, 1.0),
        decay_early_syn=slope(st, 0.1, 1.0),
        decay_late_ref=slope(rt, 1.0, 4.0),
        decay_late_syn=slope(st, 1.0, 4.0),
        centroid_ref_0_1=cen(rt, 0.1),
        centroid_syn_0_1=cen(st, 0.1),
        centroid_ref_1=cen(rt, 1.0),
        centroid_syn_1=cen(st, 1.0),
        cents_ref=ra.pitch.cents,
        cents_syn=sa.pitch.cents,
        B_ref=ra.pitch.B,
        B_syn=sa.pitch.B,
    )
    return res, ra, sa_ref


def plot_note(ax_tracks, ax_spec, ra, sa, title, npart=8):
    rt, st = ra.tracks, sa.tracks
    ref = rt.rms_db.max()
    cols = ["#1f77b4", "#ff7f0e", "#2ca02c", "#d62728", "#9467bd", "#8c564b", "#e377c2", "#7f7f7f"]
    for k in range(min(npart, len(rt.partial_n))):
        c = cols[k % len(cols)]
        ax_tracks.plot(rt.t, pa.db(rt.amp[k]) - ref, "-", color=c, lw=0.9, label=f"{rt.partial_n[k]}")
        ax_tracks.plot(st.t, pa.db(st.amp[k]) - ref, "--", color=c, lw=0.9)
    ax_tracks.set_xscale("symlog", linthresh=0.1)
    ax_tracks.set_xlim(0, min(rt.t[-1], st.t[-1]))
    ax_tracks.set_ylim(-85, 5)
    ax_tracks.set_title(title, fontsize=9)
    ax_tracks.set_xlabel("time (s)", fontsize=8)
    ax_tracks.set_ylabel("partial level (dB)", fontsize=8)
    ax_tracks.grid(alpha=0.3)
    ax_tracks.tick_params(labelsize=7)
    ax_tracks.legend(fontsize=6, ncol=2, title="partial", title_fontsize=6)
    if ax_spec is None:
        return
    for tt, c in [(0.05, "#d62728"), (0.5, "#2ca02c"), (2.5, "#1f77b4")]:
        kr = int(np.argmin(np.abs(rt.t - tt)))
        ks = int(np.argmin(np.abs(st.t - tt)))
        nn = min(len(rt.partial_n), 60)
        ax_spec.plot(rt.partial_n[:nn], pa.db(rt.amp[:nn, kr]) - ref, "-", color=c, lw=0.9, label=f"{tt:g} s")
        ax_spec.plot(st.partial_n[:nn], pa.db(st.amp[:nn, ks]) - ref, "--", color=c, lw=0.9)
    ax_spec.set_ylim(-95, 5)
    ax_spec.set_xlabel("partial number", fontsize=8)
    ax_spec.set_title("partial spectrum (solid: recording, dashed: FM)", fontsize=9)
    ax_spec.grid(alpha=0.3)
    ax_spec.tick_params(labelsize=7)
    ax_spec.legend(fontsize=6)


def write_report(d: Path, results):
    """A static HTML page listing the metrics and plots (served as docs/img/)."""
    rows = []
    for r in results:
        img = f"cmp_{r['note'].replace('#', 's')}_v{r['layer']}.png"
        rows.append(
            f"<tr><td>{r['note']}</td><td>{r['layer']}</td><td>{r['band_err_db']:.2f}</td>"
            f"<td>{r['band_bias_db']:+.2f}</td><td>{r['partial_err_db']:.2f}</td>"
            f"<td>{r['peak_syn_db'] - r['peak_ref_db']:+.1f}</td>"
            f"<td>{r['decay_early_ref']:.1f} / {r['decay_early_syn']:.1f}</td>"
            f"<td>{r['decay_late_ref']:.1f} / {r['decay_late_syn']:.1f}</td>"
            f"<td>{r['centroid_ref_0_1']:.0f} / {r['centroid_syn_0_1']:.0f}</td>"
            f"<td>{r['cents_ref']:+.1f} / {r['cents_syn']:+.1f}</td>"
            f"<td>{r['B_ref']:.2e} / {r['B_syn']:.2e}</td>"
            f"<td><a href='{img}'>plot</a></td></tr>"
        )
    be = np.mean([r["band_err_db"] for r in results])
    pe = np.mean([r["partial_err_db"] for r in results])
    imgs = "".join(
        f"<figure><img src='cmp_{r['note'].replace('#', 's')}_v{r['layer']}.png' loading='lazy' "
        f"alt='{r['note']} layer {r['layer']}'></figure>"
        for r in results
    )
    html = f"""<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1"><title>FM Piano accuracy report</title>
<style>
:root {{ --bg:#f6f4f0; --text:#1d1b18; --line:#e2ddd3; --muted:#6b665d; color-scheme: light; }}
@media (prefers-color-scheme: dark) {{ :root {{ --bg:#141310; --text:#ece8df; --line:#34302a; --muted:#a39d91; color-scheme: dark; }} }}
body {{ margin:0; padding:16px; background:var(--bg); color:var(--text); font:14px/1.5 system-ui,sans-serif; }}
main {{ max-width:1180px; margin:0 auto; }}
.tw {{ overflow-x:auto; }}
table {{ border-collapse:collapse; font-variant-numeric:tabular-nums; font-size:13px; }}
td,th {{ border-bottom:1px solid var(--line); padding:3px 8px; text-align:right; white-space:nowrap; }}
th {{ position:sticky; top:0; background:var(--bg); }}
img {{ max-width:100%; height:auto; background:#fff; border-radius:6px; }}
figure {{ margin:8px 0; }}
p {{ max-width:80ch; color:var(--muted); }}
</style></head><body><main>
<h1>FM Piano: accuracy report</h1>
<p>Renders of the Rust FM engine (dry, mono, fitted <i>Concert Grand</i> tone) analysed with the same code as
the Salamander Grand Piano V3 recordings they were fitted to. <b>Band error</b> is the RMS difference of
time-smoothed ERB-band spectrograms (dB, 10 ms to 8 s, weighted towards loud bands), <b>bias</b> its mean
(positive = FM louder), <b>partials</b> the RMS level error of partials 1–8 at 0.1, 0.5 and 2 s. Decay
rates are of the RMS envelope (dB/s) over 0.1–1 s and 1–4 s; centroid in Hz at 0.1 s; pitch of partial 1
in cents re. 12-TET; B is the measured inharmonicity. Pairs are recording / FM.</p>
<p>Mean band error {be:.2f} dB, mean partial error {pe:.2f} dB over {len(results)} notes.</p>
<p><a href="error_map.png">Error map</a> · <a href="fit_overview.png">Overview</a> · <a href="../">Back to the piano</a></p>
<div class="tw"><table><thead><tr><th>key</th><th>layer</th><th>band dB</th><th>bias</th><th>partials dB</th>
<th>peak Δ dB</th><th>decay 0.1–1 s</th><th>decay 1–4 s</th><th>centroid</th><th>cents</th><th>B</th><th></th></tr></thead>
<tbody>{"".join(rows)}</tbody></table></div>
<h2>Plots</h2><p>Left: partial envelopes, recording solid, FM dashed. Right: partial spectra at 0.05, 0.5 and 2.5 s.</p>
{imgs}
</main></body></html>"""
    (d / "index.html").write_text(html)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("synth_dir", type=Path)
    ap.add_argument("--layers", default="4,10,16")
    ap.add_argument("--notes", default="all")
    ap.add_argument("--plots", type=Path)
    ap.add_argument("--json", type=Path)
    ap.add_argument("--overview", default="A0,C2,C4,C6,C7", help="notes for the overview figure (mf)")
    a = ap.parse_args()
    layers = [int(v) for v in a.layers.split(",")]
    notes = pa.SALAMANDER_NOTES if a.notes == "all" else a.notes.split(",")
    if a.plots:
        import matplotlib

        matplotlib.use("Agg")
        import matplotlib.pyplot as plt

        a.plots.mkdir(parents=True, exist_ok=True)
    results, keep = [], {}
    for note in notes:
        for layer in layers:
            r, ra, sa = compare_one(note, layer, a.synth_dir)
            results.append(r)
            keep[(note, layer)] = (ra, sa)
            print(
                f"{note:4s} v{layer:2d}  band {r['band_err_db']:5.2f} dB (bias {r['band_bias_db']:+5.2f})"
                f"  partials {r['partial_err_db']:5.2f} dB  peak {r['peak_syn_db'] - r['peak_ref_db']:+5.1f} dB"
                f"  decay {r['decay_early_syn']:6.1f}/{r['decay_early_ref']:6.1f} dB/s"
                f"  cents {r['cents_syn'] - r['cents_ref']:+5.1f}  B {r['B_syn'] / max(r['B_ref'], 1e-9):4.2f}x",
                flush=True,
            )
            if a.plots:
                fig, (a1, a2) = plt.subplots(1, 2, figsize=(11, 3.6))
                plot_note(a1, a2, ra, sa, f"{note} layer {layer}: recording (solid) vs FM (dashed)")
                fig.tight_layout()
                fig.savefig(a.plots / f"cmp_{note.replace('#', 's')}_v{layer}.png", dpi=64)
                plt.close(fig)
    be = np.array([r["band_err_db"] for r in results])
    pe = np.array([r["partial_err_db"] for r in results])
    print(f"\nmean ERB-band error {be.mean():.2f} dB (median {np.median(be):.2f}), partials 1-8 {pe.mean():.2f} dB over {len(results)} notes")
    if a.json:
        a.json.write_text(json.dumps(results, indent=1))
    if a.plots:
        write_report(a.plots, results)
    if a.plots:
        ov = [n for n in a.overview.split(",") if (n, 10) in keep] or [notes[0]]
        lay = 10 if (ov[0], 10) in keep else layers[0]
        fig, axes = plt.subplots(1, len(ov), figsize=(3.6 * len(ov), 3.4), squeeze=False)
        for ax, n in zip(axes[0], ov):
            ra, sa = keep[(n, lay)]
            plot_note(ax, None, ra, sa, f"{n} (layer {lay})", npart=6)
        fig.tight_layout()
        fig.savefig(a.plots / "fit_overview.png", dpi=90)
        plt.close(fig)
        # error map
        nn = sorted({r["note"] for r in results}, key=pa.note_to_midi)
        M = np.full((len(layers), len(nn)), np.nan)
        for r in results:
            M[layers.index(r["layer"]), nn.index(r["note"])] = r["band_err_db"]
        fig, ax = plt.subplots(figsize=(11, 1.2 + 0.35 * len(layers)))
        im = ax.imshow(M, aspect="auto", cmap="viridis", vmin=0, vmax=8)
        ax.set_xticks(range(len(nn)), nn, fontsize=7, rotation=90)
        ax.set_yticks(range(len(layers)), [f"layer {v}" for v in layers], fontsize=7)
        for i in range(M.shape[0]):
            for j in range(M.shape[1]):
                if np.isfinite(M[i, j]):
                    ax.text(j, i, f"{M[i, j]:.1f}", ha="center", va="center", fontsize=6, color="w" if M[i, j] < 5 else "k")
        ax.set_title("ERB-band spectrogram error, FM render vs recording (dB)", fontsize=9)
        fig.colorbar(im, ax=ax, fraction=0.02)
        fig.tight_layout()
        fig.savefig(a.plots / "error_map.png", dpi=90)
        plt.close(fig)


if __name__ == "__main__":
    main()
