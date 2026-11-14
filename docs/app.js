// FM Piano front end: loads the wasm engine, runs it in an AudioWorklet,
// and drives it from an on-screen keyboard, computer keys and Web MIDI.

const P = {
  VOLUME: 0, REVERB: 1, BRIGHTNESS: 2, HARDNESS: 3, TILT: 4, DECAY: 5, DETUNE: 6,
  STRETCH: 7, INHARM: 8, HAMMER: 9, RELEASE: 10, VEL_CURVE: 11, WIDTH: 12,
  STRINGS: 13, UNISON_MIX: 14, ENGINE: 15, STRIKE: 16, NOTCH: 17, BOARD: 18,
  HIGH_DAMP: 19, DYNAMICS: 20, AFTERSOUND: 21, COUNT: 22,
};
const ENGINE_FITTED = 0, ENGINE_IDEAL = 1;
const DEFAULT_PRESET = 'Ideal Grand';

// Tone controls.  Sliders: {id, label, min, max, step, hint, fmt, log}; with
// log: true the slider moves the logarithm of the value (for multipliers).
// Selects: {id, label, options: [[value, text], ...], hint}.
const UNISON_C4_CENTS = 1.0; // measured unison spread around middle C
const times = (v) => `×${v.toFixed(2)}`;
const IDEAL_SLIDERS = [
  { id: P.STRIKE, label: 'Strike point', min: 0.05, max: 0.25, step: 0.001, log: true,
    hint: 'where the hammer hits the string; partials near multiples of 1/strike go missing', fmt: (v) => `1/${(1 / v).toFixed(1)} of the string` },
  { id: P.NOTCH, label: 'Strike notches', min: 0, max: 1, step: 0.01,
    hint: 'depth of those gaps: 0 = none, 0.5 = as on the recorded grand, 1 = deep', fmt: (v) => v.toFixed(2) },
  { id: P.BOARD, label: 'Soundboard low corner', min: 0.3, max: 4, step: 0.01, log: true,
    hint: 'higher = thinner bass fundamentals, as on a smaller instrument', fmt: times },
  { id: P.HIGH_DAMP, label: 'High-partial damping', min: 0.2, max: 5, step: 0.01, log: true,
    hint: 'how fast the upper partials die away (string losses)', fmt: times },
  { id: P.AFTERSOUND, label: 'Aftersound', min: 0.05, max: 10, step: 0.01, log: true,
    hint: 'level of the slow second stage of the decay: higher notes sing on longer', fmt: times },
  { id: P.DYNAMICS, label: 'Dynamic range', min: 0.3, max: 2, step: 0.01,
    hint: 'loudness difference between soft and hard strikes', fmt: times },
];
const SLIDERS = [
  { id: P.BRIGHTNESS, label: 'Brightness', min: 0.3, max: 2.0, step: 0.01, log: true,
    hint: 'harder, brighter tone (FM index, or the hammer contact time)', fmt: times },
  { id: P.HARDNESS, label: 'Hammer hardness', min: -1, max: 1, step: 0.01, hint: 'extra brightness for hard strikes', fmt: (v) => v.toFixed(2) },
  { id: P.TILT, label: 'Spectral tilt', min: -8, max: 6, step: 0.1, hint: 'dB per octave above the fundamental', fmt: (v) => `${v.toFixed(1)} dB/oct` },
  { id: P.DECAY, label: 'Sustain length', min: 0.2, max: 3, step: 0.01, log: true, hint: 'scales all decay times', fmt: times },
  { id: P.DETUNE, label: 'Unison detune (beating speed)', min: 0, max: 10, step: 0.05,
    hint: '× the detuning measured between each note\'s strings: ≈1 ¢ up to C5, 2.4 ¢ in the top octaves; 0 = perfectly tuned',
    fmt: (v) => `×${v.toFixed(2)} · ${(v * UNISON_C4_CENTS).toFixed(1)} ¢ at C4` },
  { id: P.UNISON_MIX, label: 'Unison balance (beating depth)', min: 0, max: 1, step: 0.01,
    hint: '0 = as recorded: prompt sound on one string, aftersound on the others; 1 = equal shares, deep beating throughout', fmt: (v) => v.toFixed(2) },
  { id: P.STRINGS, label: 'Strings per note', options: [[0, 'As on a grand: 1 · 2 · 3'], [1, '1 string (no unison)'], [2, '2 strings'], [3, '3 strings']],
    hint: 'grand: single strings A0–E1, pairs F1–E2, triples from F2' },
  { id: P.INHARM, label: 'String stiffness', min: 0, max: 5, step: 0.01, hint: 'inharmonicity B (× the measured curve)', fmt: times },
  { id: P.STRETCH, label: 'Octave stretch', min: 0, max: 2, step: 0.01, hint: '0 = equal temperament', fmt: times },
  { id: P.HAMMER, label: 'Hammer noise', min: 0, max: 3, step: 0.01, hint: 'level of the FM knock burst', fmt: times },
  { id: P.RELEASE, label: 'Damper release', min: 0.2, max: 4, step: 0.01, log: true, hint: 'time for dampers to stop a note', fmt: times },
  { id: P.VEL_CURVE, label: 'Velocity curve', min: 0.5, max: 2, step: 0.01, hint: '<1 plays louder, >1 softer', fmt: (v) => v.toFixed(2) },
  { id: P.REVERB, label: 'Room', min: 0, max: 1, step: 0.01, hint: 'reverb amount', fmt: (v) => `${Math.round(v * 100)}%` },
  { id: P.WIDTH, label: 'Stereo width', min: 0, max: 2, step: 0.01, hint: 'key panning', fmt: (v) => `${Math.round(v * 100)}%` },
  { id: P.VOLUME, label: 'Volume', min: 0, max: 1.5, step: 0.01, hint: '', fmt: (v) => `${Math.round(v * 100)}%` },
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

const presetEngine = (i) => (ui.fmp_preset_engine ? ui.fmp_preset_engine(i) : ENGINE_FITTED);

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
  for (let id = 0; id < P.COUNT; id++) send({ t: 'param', id, v: ui.fmp_get_param(uiEngine, id) });
  if (ctx.state !== 'running') await ctx.resume();
  $('power').textContent = 'Audio on';
  $('power').classList.remove('primary');
  $('status').textContent = `Running at ${ctx.sampleRate} Hz, ${Math.round((ctx.baseLatency || 0) * 1000)} ms buffer.`;
  for (const id of ['ab-fm', 'ab-ref', 'ab-both', 'ab-stop']) $(id).disabled = false;
  drawScope();
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

// ------------------------------------------------------------------ Web MIDI
const midiState = { access: null, count: 0, lines: [] };
const esc = (t) => String(t).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));

function midiChip(text, state) {
  const chip = $('midi-chip');
  chip.textContent = `MIDI: ${text}`;
  chip.dataset.state = state;
}

function midiLog(text) {
  const t = (performance.now() / 1000).toFixed(2).padStart(7);
  midiState.lines.push(`${t}  ${text}`);
  if (midiState.lines.length > 300) midiState.lines.shift();
  const el = $('midi-log');
  el.textContent = midiState.lines.join('\n');
  el.scrollTop = el.scrollHeight;
}

function describeMidi(d) {
  const [st, a, b] = d;
  const ch = (st & 0x0f) + 1;
  const cc = { 1: ' (mod wheel)', 7: ' (volume)', 64: ' (sustain pedal)', 66: ' (sostenuto)', 67: ' (soft pedal)', 120: ' (all sound off)', 123: ' (all notes off)' };
  switch (st & 0xf0) {
    case 0x80: return `ch${ch} note off ${noteName(a)} (${a}) vel ${b}`;
    case 0x90: return b ? `ch${ch} note on  ${noteName(a)} (${a}) vel ${b}` : `ch${ch} note off ${noteName(a)} (${a}) (on, vel 0)`;
    case 0xa0: return `ch${ch} key pressure ${noteName(a)} ${b}`;
    case 0xb0: return `ch${ch} control ${a} = ${b}${cc[a] || ''}`;
    case 0xc0: return `ch${ch} program change ${a + 1}`;
    case 0xd0: return `ch${ch} channel pressure ${a}`;
    case 0xe0: return `ch${ch} pitch bend ${((b << 7) | a) - 8192}`;
    default: return st === 0xf0 ? 'system exclusive' : `system message 0x${st.toString(16)}`;
  }
}

function onMidiMessage(ev) {
  const d = ev.data;
  if (!d || !d.length || d[0] === 0xf8 || d[0] === 0xfe) return; // clock, active sensing
  midiState.count++;
  $('midi-count').textContent = `${midiState.count} message${midiState.count === 1 ? '' : 's'} received`;
  const st = d[0];
  const chSel = Number($('midi-channel').value);
  const used = st < 0xf0 && (chSel < 0 || chSel === (st & 0x0f));
  const hex = Array.from(d.slice(0, 3), (x) => x.toString(16).padStart(2, '0')).join(' ');
  const many = midiState.access && midiState.access.inputs.size > 1;
  const port = many ? `[${((ev.target && ev.target.name) || 'input').slice(0, 16)}] ` : '';
  midiLog(`${hex.padEnd(8)}  ${port}${describeMidi(d)}${used || st >= 0xf0 ? '' : ' (other channel, ignored)'}`);
  if (!used) return;
  const [, a, b] = d;
  const cmd = st & 0xf0;
  if (cmd === 0x90 && b > 0) {
    if (!ctx) midiLog('    audio is off: click "Start audio" first (browsers only start sound after a click)');
    noteOn(a, b, 'm' + a);
  } else if (cmd === 0x80 || (cmd === 0x90 && b === 0)) noteOff('m' + a, a);
  else if (cmd === 0xb0 && a === 64) setSustain(b >= 64);
  else if (cmd === 0xb0 && (a === 120 || a === 123)) send({ t: 'alloff' });
}

function refreshMidiDevices() {
  const acc = midiState.access;
  const rows = [];
  if (acc) {
    for (const port of [...acc.inputs.values(), ...acc.outputs.values()]) {
      if (port.type === 'input' && port.onmidimessage !== onMidiMessage) {
        port.onmidimessage = onMidiMessage;
        if (port.open) port.open().catch((e) => midiLog(`could not open "${port.name}": ${e.name}: ${e.message}`));
      }
      const maker = port.manufacturer ? ` · ${esc(port.manufacturer)}` : '';
      rows.push(`<li><b>${esc(port.name || '?')}</b><span class="muted">${port.type}${maker} · ${port.state} · ${port.connection}</span></li>`);
    }
  }
  $('midi-devices').innerHTML = rows.length ? rows.join('')
    : `<li class="muted">${acc ? 'Access granted, but the browser reports no MIDI devices. Plug one in (the list updates by itself) or check the notes below.' : 'Not connected yet: press “Connect”.'}</li>`;
  const n = acc ? acc.inputs.size : 0;
  $('midi-summary').textContent = acc ? `${n} input${n === 1 ? '' : 's'}` : 'not connected';
  if (acc) midiChip(`${n} input${n === 1 ? '' : 's'}`, n ? 'on' : 'empty');
}

async function queryMidiPermission() {
  const out = $('midi-perm');
  if (!navigator.permissions || !navigator.permissions.query) {
    out.textContent = 'cannot be queried in this browser';
    return null;
  }
  try {
    const st = await navigator.permissions.query({ name: 'midi', sysex: $('midi-sysex').checked });
    out.textContent = st.state;
    st.onchange = () => { out.textContent = st.state; midiLog(`permission is now: ${st.state}`); };
    return st.state;
  } catch (e) {
    out.textContent = `cannot be queried in this browser (${e.name})`;
    return null;
  }
}

function midiFail(e) {
  const name = (e && e.name) || 'Error';
  $('midi-access').textContent = `refused: ${name}`;
  midiChip('refused', 'error');
  midiLog(`requestMIDIAccess failed: ${name}: ${(e && e.message) || e}`);
  const hints = {
    SecurityError: 'Permission was refused. In Firefox this is what happens when the "site permission add-on" prompt is dismissed, blocked, or not available (e.g. in a private window). See "If it does not work" below, then press Connect again.',
    NotAllowedError: 'Permission was refused. See "If it does not work" below.',
    AbortError: 'The request was cancelled before it finished (for example the add-on install was cancelled).',
    NotSupportedError: 'The browser refused this kind of request (try without SysEx).',
    InvalidStateError: 'The browser could not start its MIDI service. Close other programs that may hold the device and restart the browser.',
  };
  if (hints[name]) midiLog(`    ${hints[name]}`);
  queryMidiPermission();
}

// Must run synchronously inside a click so the browser can show its prompt
// (Firefox requires that to offer its site-permission add-on).
function connectMidi(fromClick) {
  if (!('requestMIDIAccess' in navigator)) {
    midiLog('navigator.requestMIDIAccess does not exist in this browser.');
    return;
  }
  const sysex = $('midi-sysex').checked;
  midiLog(`requesting MIDI access${sysex ? ' (with SysEx)' : ''}…`);
  $('midi-access').textContent = 'waiting for the browser (look for a prompt near the address bar)';
  midiChip('asking…', 'wait');
  let req;
  try {
    req = navigator.requestMIDIAccess({ sysex });
  } catch (e) {
    midiFail(e);
    return;
  }
  if (fromClick && !ctx) startAudio(); // the same click can also unlock sound
  req.then((acc) => {
    midiState.access = acc;
    acc.onstatechange = (e) => {
      midiLog(`${e.port.type} "${e.port.name}" is ${e.port.state} / ${e.port.connection}`);
      refreshMidiDevices();
    };
    $('midi-access').textContent = `granted${acc.sysexEnabled ? ' (SysEx too)' : ''}`;
    midiLog(`access granted: ${acc.inputs.size} input(s), ${acc.outputs.size} output(s)`);
    refreshMidiDevices();
    queryMidiPermission();
  }, midiFail);
}

async function initMidiPanel() {
  const hasApi = 'requestMIDIAccess' in navigator;
  const ua = navigator.userAgent;
  const firefox = /firefox\//i.test(ua);
  $('midi-api').textContent = hasApi ? 'available' : (/safari/i.test(ua) && !/chrome|chromium|android/i.test(ua) ? 'not available: Safari does not implement Web MIDI' : 'not available in this browser');
  $('midi-secure').textContent = window.isSecureContext ? 'yes' : 'no: Web MIDI only works on https:// pages (or localhost)';
  $('midi-browser').textContent = ua;

  const ch = $('midi-channel');
  for (let i = 0; i < 16; i++) ch.insertAdjacentHTML('beforeend', `<option value="${i}">${i + 1}</option>`);
  $('midi-connect').disabled = !hasApi;
  if (!hasApi) midiChip('unsupported', 'error');
  $('midi-connect').addEventListener('click', () => connectMidi(true));
  $('midi-clear').addEventListener('click', () => { midiState.lines = []; midiState.count = 0; $('midi-log').textContent = ''; $('midi-count').textContent = ''; });
  $('midi-sysex').addEventListener('change', queryMidiPermission);
  refreshMidiDevices();
  if (!hasApi) return;
  const state = await queryMidiPermission();
  // Firefox users usually need the add-on notes until access works once
  if (firefox && state !== 'granted') $('midi-help-firefox').open = true;
  if (state === 'granted') {
    midiLog('MIDI permission was granted before: connecting without a prompt.');
    connectMidi(false);
  } else {
    midiLog('Press "Connect" to ask the browser for access.');
  }
}

// ------------------------------------------------------------------ tone UI
const sliderEls = {};
function setParam(id, v) {
  ui.fmp_set_param(uiEngine, id, v);
  send({ t: 'param', id, v });
}

function buildSliders(box, defs) {
  for (const d of defs) {
    const wrap = document.createElement('label');
    wrap.className = 'slider';
    if (d.options) {
      wrap.innerHTML = `<span>${d.label}</span><output></output><select>${d.options.map(([v, t]) => `<option value="${v}">${t}</option>`).join('')}</select><small>${d.hint}</small>`;
      const sel = wrap.querySelector('select');
      sel.addEventListener('change', () => setParam(d.id, Number(sel.value)));
      sliderEls[d.id] = { set: (v) => { sel.value = v; } };
      box.appendChild(wrap);
      continue;
    }
    const lo = d.log ? Math.log(d.min) : d.min, hi = d.log ? Math.log(d.max) : d.max;
    const step = d.log ? (hi - lo) / 400 : d.step;
    wrap.innerHTML = `<span>${d.label}</span><output></output><input type="range" min="${lo}" max="${hi}" step="${step}">${d.hint ? `<small>${d.hint}</small>` : ''}`;
    const input = wrap.querySelector('input');
    const out = wrap.querySelector('output');
    const value = () => (d.log ? Math.exp(Number(input.value)) : Number(input.value));
    input.addEventListener('input', () => {
      const v = value();
      out.textContent = d.fmt(v);
      setParam(d.id, v);
    });
    sliderEls[d.id] = {
      set: (v) => {
        input.value = d.log ? Math.log(Math.max(v, 1e-6)) : v;
        out.textContent = d.fmt(v);
      },
    };
    box.appendChild(wrap);
  }
}

function showEngine(engine) {
  for (const r of document.querySelectorAll('input[name=engine]')) r.checked = Number(r.value) === engine;
  $('sliders-ideal').hidden = engine !== ENGINE_IDEAL;
}

function refreshSliders() {
  for (const [id, s] of Object.entries(sliderEls)) s.set(ui.fmp_get_param(uiEngine, Number(id)));
  showEngine(Math.round(ui.fmp_get_param(uiEngine, P.ENGINE)));
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

function initEngineSwitch() {
  for (const r of document.querySelectorAll('input[name=engine]')) {
    r.addEventListener('change', () => {
      if (!r.checked) return;
      setParam(P.ENGINE, Number(r.value));
      showEngine(Number(r.value));
    });
  }
}

function fillPresets(sel) {
  const names = presetNames();
  const groups = [[ENGINE_IDEAL, 'Idealised piano'], [ENGINE_FITTED, 'Fitted to the recording']];
  for (const [engine, label] of groups) {
    const g = document.createElement('optgroup');
    g.label = label;
    names.forEach((name, i) => {
      if (presetEngine(i) !== engine) return;
      const o = document.createElement('option');
      o.value = i;
      o.textContent = name;
      g.appendChild(o);
    });
    if (g.children.length) sel.appendChild(g);
  }
  const def = names.indexOf(DEFAULT_PRESET);
  sel.value = String(def >= 0 ? def : 0);
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

function renderFm(key, layer, secs, preset) {
  const sr = ctx.sampleRate;
  const e = ui.fmp_new(sr);
  ui.fmp_set_preset(e, preset);
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
  const preset = Number($('ab-engine').value || 0);
  const t0 = ctx.currentTime + 0.05;
  if (which === 'fm') playBuffer(renderFm(key, layer, 6, preset), t0);
  else if (which === 'ref') playBuffer(await loadSample(key, layer), t0);
  else {
    const ref = await loadSample(key, layer);
    const end = playBuffer(renderFm(key, layer, 6, preset), ctx.currentTime + 0.05);
    playBuffer(ref, end + 0.5);
  }
}

// ------------------------------------------------------------------ init
async function main() {
  buildKeyboard();
  initComputerKeys();
  buildSliders($('sliders-ideal'), IDEAL_SLIDERS);
  buildSliders($('sliders'), SLIDERS);
  initEngineSwitch();
  initMidiPanel();
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
  fillPresets(sel);
  sel.disabled = false;
  sel.addEventListener('change', () => applyPreset(Number(sel.value)));
  $('reset').addEventListener('click', () => applyPreset(Number(sel.value)));
  applyPreset(Number(sel.value));
  const abSel = $('ab-engine');
  presetNames().forEach((name, i) => {
    if (name === 'Concert Grand' || name === DEFAULT_PRESET) abSel.insertAdjacentHTML('beforeend', `<option value="${i}">${name}</option>`);
  });
  const power = $('power');
  power.disabled = false;
  power.textContent = '▶ Start audio';
  power.addEventListener('click', startAudio);
}

main();
