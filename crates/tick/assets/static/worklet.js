// Forward microphone samples (first channel) to the page in 2048 sample
// blocks (spec/tick.md TICK-3).
const BLOCK = 2048;

class TickCapture extends AudioWorkletProcessor {
  constructor() {
    super();
    this.buf = new Float32Array(BLOCK);
    this.len = 0;
  }

  process(inputs) {
    const ch = inputs[0] && inputs[0][0];
    if (ch) {
      for (let i = 0; i < ch.length; i++) {
        this.buf[this.len++] = ch[i];
        if (this.len === BLOCK) {
          this.port.postMessage(this.buf, [this.buf.buffer]);
          this.buf = new Float32Array(BLOCK);
          this.len = 0;
        }
      }
    }
    return true;
  }
}

registerProcessor("tick-capture", TickCapture);
