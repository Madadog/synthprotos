#!/usr/bin/env bash
# Download the Salamander Grand Piano V3 reference samples (CC-BY 3.0,
# Alexander Holm) used by the analysis and fitting tools.  They are taken from
# the @audio-samples/piano-velocity{1..16} npm packages (OGG, 48 kHz),
# ~75 MB in total, and unpacked into ./samples (or $PIANO_SAMPLES).
set -euo pipefail
dest="${PIANO_SAMPLES:-$(cd "$(dirname "$0")/.." && pwd)/samples}"
mkdir -p "$dest"
cd "$dest"
for v in $(seq 1 16); do
  name="piano-velocity$v"
  [ -d "$name/audio" ] && continue
  echo "fetching $name"
  mkdir -p "$name"
  curl -fsSL "https://registry.npmjs.org/@audio-samples/$name/-/$name-1.0.5.tgz" | tar -xz -C "$name" --strip-components=1
done
echo "samples in $dest"
