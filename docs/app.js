// FM Piano front end: loads the wasm engine, runs it in an AudioWorklet,
// and drives it from an on-screen keyboard, computer keys and Web MIDI.

const P = {
  VOLUME: 0, REVERB: 1, BRIGHTNESS: 2, HARDNESS: 3, TILT: 4, DECAY: 5, DETUNE: 6,
  STRETCH: 7, INHARM: 8, HAMMER: 9, RELEASE: 10, VEL_CURVE: 11, WIDTH: 12,
};

// slider definitions: [param, label, min, max, step, hint, format]
const SLIDERS = [
  [P.BRIGHTNESS, 'Brightness', 0.3, 2.0, 0.01, 'scales every FM modulation index', (v) => `×${v.toFixed(2)}`],
  [P.HARDNESS, 'Hammer hardness', -1, 1, 0.01, 'extra brightness for hard strikes', (v) => v.toFixed(2)],
  [P.TILT, 'Spectral tilt', -8, 6, 0.1, 'dB per octave above the fundamental', (v) => `${v.toFixed(1)} dB/oct`],
  [P.DECAY, 'Sustain length', 0.2, 3, 0.01, 'scales all decay times', (v) => `×${v.toFixed(2)}`],
  [P.DETUNE, 'Unison detune', 0, 30, 0.1, 'cents between the two carrier copies', (v) => `${v.toFixed(1)} ¢`],
  [P.INHARM, 'String stiffness', 0, 5, 0.01, 'inharmonicity B (×measured)', (v) => `×${v.toFixed(2)}`],
  [P.STRETCH, 'Octave stretch', 0, 2, 0.01, '0 = equal temperament', (v) => `×${v.toFixed(2)}`],
  [P.HAMMER, 'Hammer noise', 0, 3, 0.01, 'level of the FM knock burst', (v) => `×${v.toFixed(2)}`],
  [P.RELEASE, 'Damper release', 0.2, 4, 0.01, 'time for dampers to stop a note', (v) => `×${v.toFixed(2)}`],
  [P.VEL_CURVE, 'Velocity curve', 0.5, 2, 0.01, '<1 plays louder, >1 softer', (v) => v.toFixed(2)],
  [P.REVERB, 'Room', 0, 1, 0.01, 'reverb amount', (v) => `${Math.round(v * 100)}%`],
  [P.WIDTH, 'Stereo width', 0, 2, 0.01, 'key panning', (v) => `${Math.round(v * 100)}%`],
  [P.VOLUME, 'Volume', 0, 1.5, 0.01, '', (v) => `${Math.round(v * 100)}%`],
];

const NOTE_NAMES = ['C', 'C#', 'D', 'D#', 'E', 'F', 'F#', 'G', 'G#', 'A', 'A#', 'B'];
const noteName = (m) => NOTE_NAMES[m % 12] + (Math.floor(m / 12) - 1);
const isBlack = (m) => [1, 3, 6, 8, 10].includes(m % 12);
const REF_KEYS = [21, ...[1, 2, 3, 4, 5, 6, 7].flatMap((o) => [0, 3, 6, 9].map((k) => 12 * (o + 1) + k)), 108];
const fileName = (m) => noteName(m).replace('#', 's');
const layerVelocity = (layer) => 8 + (layer - 1) * (127 - 8) / 15;
const UNITY_VOLUME = 1 / (1.6 * Math.SQRT1_2);

const $ = (id) => document.getElementById(id);
let wasmBytes = null;
let ui = null; // main-thread engine instance: preset/param state + offline renders
let uiEngine = 0;
let ctx = null, node = null, analyser = null;
let sustainDown = false;
let baseOctave = 48;
const held = new Map(); // source id -> key

// ------------------------------------------------------------------ wasm
async function loadWasm() {
  const res = await fetch('fmpiano.wasm');
  if (!res.ok) throw new Error(`fmpiano.wasm: HTTP ${res.status}`);
  wasmBytes = await res.arrayBuffer();
  const { instance } = await WebAssembly.instantiate(wasmBytes, {});
  ui = instance.exports;
  uiEngine = ui.fmp_new(48000);
}

function presetNames() {
  const names = [];
  const dec = new TextDecoder();
  for (let i = 0; i < ui.fmp_preset_count(); i++) {
    const p = ui.fmp_preset_name(i), n = ui.fmp_preset_name_len(i);
    names.push(dec.decode(new Uint8Array(ui.memory.buffer, p, n)));
  }
  return names;
}

const send = (m) => { if (node) node.port.postMessage(m); };

// ------------------------------------------------------------------ audio
async function startAudio() {
  if (ctx) {
    if (ctx.state !== 'running') await ctx.resume();
    return;
  }
  const AC = window.AudioContext || window.webkitAudioContext;
  if (!AC || !('audioWorklet' in AC.prototype)) {
    $('status').textContent = 'This browser does not support AudioWorklet, which the synth needs (try a current Chrome, Firefox or Safari).';
    return;
  }
  ctx = new AC({ latencyHint: 'interactive' });
  await ctx.audioWorklet.addModule('worklet.js');
  node = new AudioWorkletNode(ctx, 'fm-piano', {
    numberOfInputs: 0,
    numberOfOutputs: 1,
    outputChannelCount: [2],
    processorOptions: { wasm: wasmBytes.slice(0) },
  });
  node.port.onmessage = (e) => {
    if (e.data.t === 'stats') $('voices').textContent = `${e.data.voices} voice${e.data.voices === 1 ? '' : 's'}`;
  };
  analyser = ctx.createAnalyser();
  analyser.fftSize = 4096;
  analyser.smoothingTimeConstant = 0.6;
  node.connect(ctx.destination);
  node.connect(analyser);
  // mirror the UI state into the audio engine
  send({ t: 'preset', i: Number($('preset').value) });
  for (let id = 0; id <= P.WIDTH; id++) send({ t: 'param', id, v: ui.fmp_get_param(uiEngine, id) });
  if (ctx.state !== 'running') await ctx.resume();
  $('power').textContent = 'Audio on';
  $('power').classList.remove('primary');
  $('status').textContent = `Running at ${ctx.sampleRate} Hz, ${Math.round((ctx.baseLatency || 0) * 1000)} ms buffer.`;
  for (const id of ['ab-fm', 'ab-ref', 'ab-both', 'ab-stop']) $(id).disabled = false;
  drawScope();
  initMidi();
}

function noteOn(key, vel, src) {
  if (!ctx) return;
  if (ctx.state !== 'running') ctx.resume();
  if (src !== undefined) {
    const prev = held.get(src);
    if (prev !== undefined && prev !== key) noteOff(src);
    held.set(src, key);
  }
  send({ t: 'on', k: key, v: Math.max(1, Math.min(127, vel)) });
  keyEls[key]?.classList.add('down');
}

function noteOff(src, keyOverride) {
  const key = keyOverride ?? held.get(src);
  if (src !== undefined) held.delete(src);
  if (key === undefined) return;
  // keep the key lit if another source still holds it
  for (const k of held.values()) if (k === key) return;
  send({ t: 'off', k: key });
  keyEls[key]?.classList.remove('down');
}

function setSustain(down) {
  sustainDown = down;
  $('sustain').setAttribute('aria-pressed', String(down));
  send({ t: 'sus', d: down });
}

// ------------------------------------------------------------------ keyboard
const keyEls = {};
function buildKeyboard() {
  const kb = $('keyboard');
  const whiteW = window.matchMedia('(pointer: coarse)').matches ? 34 : 24;
  let x = 0;
  const whites = [];
  for (let m = 21; m <= 108; m++) {
    const el = document.createElement('div');
    el.dataset.key = m;
    el.className = 'key ' + (isBlack(m) ? 'black' : 'white');
    if (REF_KEYS.includes(m)) {
      el.classList.add('ref');
      el.title = `${noteName(m)} (recorded key, see comparison below)`;
    }
    if (isBlack(m)) {
      const w = whiteW * 0.62;
      el.style.left = `${x - w / 2}px`;
      el.style.width = `${w}px`;
    } else {
      el.style.left = `${x}px`;
      el.style.width = `${whiteW}px`;
      if (m % 12 === 0) el.innerHTML = `<span class="lbl">${noteName(m)}</span>`;
      x += whiteW;
      whites.push(el);
    }
    keyEls[m] = el;
    kb.appendChild(el);
  }
  kb.style.width = `${x}px`;
  const wrap = $('kbd-wrap');
  requestAnimationFrame(() => {
    // show C4 upwards; centre C3..C6 when there is room
    const c3 = keyEls[48].offsetLeft, c6 = keyEls[84].offsetLeft;
    const room = wrap.clientWidth - (c6 - c3);
    wrap.scrollLeft = room > 0 ? c3 - room / 2 : keyEls[60].offsetLeft - 4;
  });
  $('kbd-left').onclick = () => wrap.scrollBy({ left: -7 * whiteW * 2, behavior: 'smooth' });
  $('kbd-right').onclick = () => wrap.scrollBy({ left: 7 * whiteW * 2, behavior: 'smooth' });

  const velocityAt = (el, clientY) => {
    if (!$('velpos').checked) return Number($('velocity').value);
    const r = el.getBoundingClientRect();
    const f = Math.min(1, Math.max(0, (clientY - r.top) / r.height));
    return Math.round(18 + 109 * f); // near the tip of the key = loud
  };
  const keyAt = (x, y) => {
    const el = document.elementFromPoint(x, y);
    return el && el.dataset && el.dataset.key ? el : null;
  };
  kb.addEventListener('pointerdown', async (e) => {
    if (!ctx) await startAudio();
    const el = keyAt(e.clientX, e.clientY);
    if (!el) return;
    kb.setPointerCapture(e.pointerId);
    noteOn(Number(el.dataset.key), velocityAt(el, e.clientY), 'p' + e.pointerId);
    e.preventDefault();
  });
  kb.addEventListener('pointermove', (e) => {
    const src = 'p' + e.pointerId;
    if (!held.has(src)) return;
    const el = keyAt(e.clientX, e.clientY);
    if (!el) return;
    const k = Number(el.dataset.key);
    if (k !== held.get(src)) noteOn(k, velocityAt(el, e.clientY), src);
  });
  const up = (e) => noteOff('p' + e.pointerId);
  kb.addEventListener('pointerup', up);
  kb.addEventListener('pointercancel', up);
}

// computer keyboard: physical key codes, two rows like a piano
const CODE_MAP = {
  KeyZ: 0, KeyS: 1, KeyX: 2, KeyD: 3, KeyC: 4, KeyV: 5, KeyG: 6, KeyB: 7, KeyH: 8, KeyN: 9, KeyJ: 10, KeyM: 11,
  Comma: 12, KeyL: 13, Period: 14, Semicolon: 15, Slash: 16,
  KeyQ: 12, Digit2: 13, KeyW: 14, Digit3: 15, KeyE: 16, KeyR: 17, Digit5: 18, KeyT: 19, Digit6: 20, KeyY: 21,
  Digit7: 22, KeyU: 23, KeyI: 24, Digit9: 25, KeyO: 26, Digit0: 27, KeyP: 28, BracketLeft: 29, Equal: 30, BracketRight: 31,
};
function initComputerKeys() {
  const typing = (e) => ['INPUT', 'SELECT', 'TEXTAREA'].includes(e.target.tagName) && e.target.type !== 'range' && e.target.type !== 'checkbox';
  window.addEventListener('keydown', async (e) => {
    if (e.metaKey || e.ctrlKey || e.altKey || typing(e)) return;
    if (e.code === 'Space') {
      e.preventDefault();
      if (!e.repeat) setSustain(true);
      return;
    }
    if (e.code === 'ArrowLeft' || e.code === 'ArrowRight') {
      e.preventDefault();
      baseOctave = Math.max(24, Math.min(84, baseOctave + (e.code === 'ArrowLeft' ? -12 : 12)));
      $('octname').textContent = noteName(baseOctave);
      return;
    }
    const off = CODE_MAP[e.code];
    if (off === undefined) return;
    e.preventDefault();
    if (e.repeat) return;
    if (!ctx) await startAudio();
    const key = baseOctave + off;
    if (key >= 21 && key <= 108) noteOn(key, Number($('velocity').value), 'k' + e.code);
  });
  window.addEventListener('keyup', (e) => {
    if (e.code === 'Space') {
      setSustain(false);
      return;
    }
    if (CODE_MAP[e.code] !== undefined) noteOff('k' + e.code);
  });
  window.addEventListener('blur', () => {
    for (const src of [...held.keys()]) noteOff(src);
    if (sustainDown) setSustain(false);
  });
}

// Web MIDI
async function initMidi() {
  if (!navigator.requestMIDIAccess) return;
  try {
    const access = await navigator.requestMIDIAccess();
    const hook = () => {
      let n = 0;
      for (const input of access.inputs.values()) {
        n++;
        input.onmidimessage = (msg) => {
          const [st, a, b] = msg.data;
          const cmd = st & 0xf0;
          if (cmd === 0x90 && b > 0) noteOn(a, b, 'm' + a);
          else if (cmd === 0x80 || (cmd === 0x90 && b === 0)) noteOff('m' + a, a);
          else if (cmd === 0xb0 && a === 64) setSustain(b >= 64);
          else if (cmd === 0xb0 && (a === 120 || a === 123)) send({ t: 'alloff' });
        };
      }
      if (n) $('status').textContent += ` MIDI: ${n} input${n > 1 ? 's' : ''} connected.`;
    };
    hook();
    access.onstatechange = hook;
  } catch (_) {
    /* MIDI permission denied or unavailable */
  }
}

// ------------------------------------------------------------------ tone UI
const sliderEls = {};
function buildSliders() {
  const box = $('sliders');
  for (const [id, label, min, max, step, hint, fmt] of SLIDERS) {
    const wrap = document.createElement('label');
    wrap.className = 'slider';
    wrap.innerHTML = `<span>${label}</span><output></output><input type="range" min="${min}" max="${max}" step="${step}">${hint ? `<small>${hint}</small>` : ''}`;
    const input = wrap.querySelector('input');
    const out = wrap.querySelector('output');
    input.addEventListener('input', () => {
      const v = Number(input.value);
      out.textContent = fmt(v);
      ui.fmp_set_param(uiEngine, id, v);
      send({ t: 'param', id, v });
    });
    sliderEls[id] = { input, out, fmt };
    box.appendChild(wrap);
  }
}

function refreshSliders() {
  for (const [id, s] of Object.entries(sliderEls)) {
    const v = ui.fmp_get_param(uiEngine, Number(id));
    s.input.value = v;
    s.out.textContent = s.fmt(v);
  }
}

function applyPreset(i) {
  // preset changes the tone; keep the global mix settings
  const keep = [P.VOLUME, P.REVERB, P.WIDTH].map((id) => [id, ui.fmp_get_param(uiEngine, id)]);
  ui.fmp_set_preset(uiEngine, i);
  for (const [id, v] of keep) ui.fmp_set_param(uiEngine, id, v);
  send({ t: 'preset', i });
  for (const [id, v] of keep) send({ t: 'param', id, v });
  refreshSliders();
}

// ------------------------------------------------------------------ spectrum
function drawScope() {
  const cv = $('scope');
  const g = cv.getContext('2d');
  const data = new Float32Array(analyser.frequencyBinCount);
  const css = getComputedStyle(document.documentElement);
  const frame = () => {
    const dpr = window.devicePixelRatio || 1;
    const w = cv.clientWidth, h = cv.clientHeight;
    if (cv.width !== Math.round(w * dpr)) { cv.width = Math.round(w * dpr); cv.height = Math.round(h * dpr); }
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    g.clearRect(0, 0, w, h);
    analyser.getFloatFrequencyData(data);
    const ny = ctx.sampleRate / 2;
    const f0 = 25, f1 = 16000;
    g.beginPath();
    for (let px = 0; px <= w; px++) {
      const f = f0 * Math.pow(f1 / f0, px / w);
      const bin = Math.min(data.length - 1, Math.round(f / ny * data.length));
      const db = Math.max(-110, data[bin]);
      const y = h - (db + 110) / 100 * h;
      px ? g.lineTo(px, y) : g.moveTo(px, y);
    }
    g.strokeStyle = css.getPropertyValue('--accent');
    g.lineWidth = 1.5;
    g.stroke();
    requestAnimationFrame(frame);
  };
  frame();
}

// ------------------------------------------------------------------ A/B comparison
let abSources = [];
const sampleCache = new Map();

function renderFm(key, layer, secs) {
  const sr = ctx.sampleRate;
  const e = ui.fmp_new(sr);
  ui.fmp_set_preset(e, 0);
  ui.fmp_set_param(e, P.REVERB, 0);
  ui.fmp_set_param(e, P.WIDTH, 0);
  ui.fmp_set_param(e, P.VOLUME, UNITY_VOLUME);
  ui.fmp_note_on(e, key, layerVelocity(layer));
  const n = Math.round(secs * sr);
  const out = new Float32Array(n);
  for (let pos = 0; pos < n; pos += 1024) {
    const m = Math.min(1024, n - pos);
    ui.fmp_render(e, m);
    const L = new Float32Array(ui.memory.buffer, ui.fmp_out_left(e), m);
    const R = new Float32Array(ui.memory.buffer, ui.fmp_out_right(e), m);
    for (let i = 0; i < m; i++) out[pos + i] = 0.5 * (L[i] + R[i]);
  }
  ui.fmp_free(e);
  fadeOut(out, sr);
  const buf = ctx.createBuffer(1, n, sr);
  buf.copyToChannel(out, 0);
  return buf;
}

function fadeOut(x, sr) {
  const nf = Math.round(0.3 * sr);
  for (let i = 0; i < nf; i++) x[x.length - 1 - i] *= i / nf;
}

async function loadSample(key, layer) {
  const id = `${fileName(key)}v${layer}`;
  if (sampleCache.has(id)) return sampleCache.get(id);
  const res = await fetch(`samples/${id}.mp3`);
  const buf = await ctx.decodeAudioData(await res.arrayBuffer());
  const ch = buf.getChannelData(0);
  fadeOut(ch, buf.sampleRate);
  sampleCache.set(id, buf);
  return buf;
}

function stopAb() {
  for (const s of abSources) try { s.stop(); } catch (_) { /* already stopped */ }
  abSources = [];
}

function playBuffer(buf, when) {
  const s = ctx.createBufferSource();
  s.buffer = buf;
  s.connect(ctx.destination);
  s.connect(analyser);
  s.start(when);
  abSources.push(s);
  return when + buf.duration;
}

async function ab(which) {
  await startAudio();
  stopAb();
  const key = Number($('ab-note').value), layer = Number($('ab-dyn').value);
  const t0 = ctx.currentTime + 0.05;
  if (which === 'fm') playBuffer(renderFm(key, layer, 6), t0);
  else if (which === 'ref') playBuffer(await loadSample(key, layer), t0);
  else {
    const ref = await loadSample(key, layer);
    const end = playBuffer(renderFm(key, layer, 6), ctx.currentTime + 0.05);
    playBuffer(ref, end + 0.5);
  }
}

// ------------------------------------------------------------------ init
async function main() {
  buildKeyboard();
  initComputerKeys();
  buildSliders();
  const vel = $('velocity');
  vel.addEventListener('input', () => { $('velout').textContent = vel.value; });
  $('sustain').addEventListener('click', async () => { if (!ctx) await startAudio(); setSustain(!sustainDown); });
  for (const m of REF_KEYS) {
    const o = document.createElement('option');
    o.value = m;
    o.textContent = noteName(m);
    if (m === 60) o.selected = true;
    $('ab-note').appendChild(o);
  }
  $('ab-fm').onclick = () => ab('fm');
  $('ab-ref').onclick = () => ab('ref');
  $('ab-both').onclick = () => ab('both');
  $('ab-stop').onclick = stopAb;
  try {
    await loadWasm();
  } catch (err) {
    $('power').textContent = 'Failed to load';
    $('status').textContent = String(err);
    return;
  }
  const sel = $('preset');
  presetNames().forEach((name, i) => {
    const o = document.createElement('option');
    o.value = i;
    o.textContent = name;
    sel.appendChild(o);
  });
  sel.disabled = false;
  sel.addEventListener('change', () => applyPreset(Number(sel.value)));
  $('reset').addEventListener('click', () => applyPreset(Number(sel.value)));
  refreshSliders();
  const power = $('power');
  power.disabled = false;
  power.textContent = '▶ Start audio';
  power.addEventListener('click', startAudio);
}

main();
