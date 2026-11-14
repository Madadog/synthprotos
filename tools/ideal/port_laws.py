"""Write the calibrated law coefficients into crates/fmpiano/src/ideal.rs.

usage: python port_laws.py [calibration.json]   (default: tools/ideal/calibration.json)
"""
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
cal = json.loads(Path(sys.argv[1] if len(sys.argv) > 1 else ROOT / "tools/ideal/calibration.json").read_text())
p = cal["params"]
rs_path = ROOT / "crates/fmpiano/src/ideal.rs"
s = open(rs_path).read()
a = s.index("// -------------------------------------------------- calibrated smooth laws")
b = s.index("// smooth fits to the measured inharmonicity")
f = lambda v: (lambda t: t if any(c in t for c in ".e") else t + ".0")(f"{v:.6g}" if abs(v) >= 1e-4 else f"{v:.6e}")
arr = lambda names: "[" + ", ".join(f(p[n]) for n in names) + "]"
block = f"""// -------------------------------------------------- calibrated smooth laws
// tools/ideal/calibration.json: one joint fit to the partial envelopes of the
// recordings (30 keys x 5 velocity layers, ERB bands, {cal['rms_db']:.1f} dB weighted rms).
// Polynomials in u = (key - 64) / 44, lowest order first.
const LEVEL: [f64; 4] = {arr(['g0','g1','g2','g3'])};
const LEVEL_VEL: [f64; 3] = {arr(['d0','d1','d2'])};
/// Curvature of level over dB(velocity): + dv2 * V² / 20 dB, V = 20 log10 v.
const LEVEL_VEL2: f64 = {f(p['dv2'])};
const CONTACT: [f64; 3] = {arr(['t0','t1','t2'])};
const CONTACT_VEL: [f64; 2] = {arr(['p0','p1'])};
/// Curvature of ln(contact time) over ln(velocity).
const CONTACT_VEL2: f64 = {f(p['pv2'])};
const HAMMER_Q: [f64; 2] = {arr(['q0','q1'])};
const GAMMA: [f64; 2] = {arr(['gam0','gam1'])};
const BOARD_LO: f64 = {f(p['lflo'])}; // ln Hz
const BOARD_HI: f64 = {f(p['lfhi'])}; // ln Hz
const BOARD_ORDER: f64 = {f(p['bk'])};
const LOSS_B1: [f64; 4] = {arr(['c0','c1','c2','c3'])};
const LOSS_B3: [f64; 3] = {arr(['e0','e1','e2'])};
const AFTER_SHARE: [f64; 4] = {arr(['w0','w1','w2','w3'])};
const PROMPT_SPEED: [f64; 4] = {arr(['r0','r1','r2','r3'])};
/// Strike-point comb (strike 1/8.5): ln of the notch-filling term eps(u).
const COMB_EPS: [f64; 2] = {arr(['ce0','ce1'])};
"""
s = s[:a] + block + s[b:]
open(rs_path, "w").write(s)
print(block)
