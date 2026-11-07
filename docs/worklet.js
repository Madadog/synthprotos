// AudioWorklet that runs the Rust FM piano (compiled to wasm32) in the audio thread.
// The wasm bytes arrive through processorOptions; events arrive on the port.

class FmPianoProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super();
    const bytes = options.processorOptions.wasm;
    const mod = new WebAssembly.Module(bytes);
    this.wasm = new WebAssembly.Instance(mod, {}).exports;
    this.engine = this.wasm.fmp_new(sampleRate);
    this.queue = [];
    this.statTimer = 0;
    this.port.onmessage = (e) => this.queue.push(e.data);
  }

  handle(m) {
    const w = this.wasm, e = this.engine;
    switch (m.t) {
      case 'on': w.fmp_note_on(e, m.k, m.v); break;
      case 'off': w.fmp_note_off(e, m.k); break;
      case 'sus': w.fmp_sustain(e, m.d ? 1 : 0); break;
      case 'preset': w.fmp_set_preset(e, m.i); break;
      case 'param': w.fmp_set_param(e, m.id, m.v); break;
      case 'alloff': w.fmp_all_off(e); break;
    }
  }

  process(inputs, outputs) {
    for (const m of this.queue) this.handle(m);
    this.queue.length = 0;
    const out = outputs[0];
    const n = out[0].length;
    const w = this.wasm;
    w.fmp_render(this.engine, n);
    const mem = w.memory.buffer;
    out[0].set(new Float32Array(mem, w.fmp_out_left(this.engine), n));
    if (out.length > 1) out[1].set(new Float32Array(mem, w.fmp_out_right(this.engine), n));
    this.statTimer += n;
    if (this.statTimer >= 4096) {
      this.statTimer = 0;
      this.port.postMessage({ t: 'stats', voices: w.fmp_active_voices(this.engine) });
    }
    return true;
  }
}

registerProcessor('fm-piano', FmPianoProcessor);
