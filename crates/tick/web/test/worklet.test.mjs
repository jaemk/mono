// The capture worklet batches 128 sample render quanta into 2048 sample
// blocks (spec/tick.md TICK-3).
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";

const src = readFileSync(new URL("../../assets/static/worklet.js", import.meta.url), "utf8");

function load() {
  const registered = {};
  const sent = [];
  class AudioWorkletProcessor {
    constructor() {
      this.port = { postMessage: (data, transfer) => sent.push({ data, transfer }) };
    }
  }
  const context = vm.createContext({
    AudioWorkletProcessor,
    Float32Array,
    registerProcessor: (name, cls) => (registered[name] = cls),
  });
  vm.runInContext(src, context, { filename: "worklet.js" });
  return { Processor: registered["tick-capture"], sent };
}

test("registers tick-capture and posts full blocks in order", () => {
  const { Processor, sent } = load();
  assert.ok(Processor);
  const p = new Processor();
  let next = 0;
  for (let q = 0; q < 20; q++) {
    const quantum = new Float32Array(128).map(() => next++);
    assert.equal(p.process([[quantum]]), true);
  }
  assert.equal(sent.length, 1);
  const { data, transfer } = sent[0];
  assert.equal(data.length, 2048);
  assert.equal(data[0], 0);
  assert.equal(data[2047], 2047);
  // the block's buffer is transferred, not copied
  assert.equal(transfer.length, 1);
  assert.equal(transfer[0], data.buffer);
});

test("keeps running without input", () => {
  const { Processor, sent } = load();
  const p = new Processor();
  assert.equal(p.process([]), true);
  assert.equal(p.process([[]]), true);
  assert.equal(sent.length, 0);
});
