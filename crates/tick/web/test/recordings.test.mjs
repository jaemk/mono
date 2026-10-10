// The detector against 30 s recordings of a real 18000 bph watch running
// near +100 s/d with about 4 ms beat error, one in a quiet room and one with
// background noise (fixtures/, 16-bit mono wav).
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const D = require("../../assets/static/detector.js");

/** A mono 16-bit pcm or 32-bit float wav as `{ sampleRate, samples }`. */
function readWav(name) {
  const buf = readFileSync(new URL(`../fixtures/${name}`, import.meta.url));
  const format = buf.readUInt16LE(20);
  const sampleRate = buf.readUInt32LE(24);
  let off = 12;
  while (buf.toString("ascii", off, off + 4) !== "data") off += 8 + buf.readUInt32LE(off + 4);
  const bytes = buf.readUInt32LE(off + 4);
  const width = format === 3 ? 4 : 2;
  const samples = new Float32Array(bytes / width);
  for (let i = 0; i < samples.length; i++) {
    const at = off + 8 + i * width;
    samples[i] = format === 3 ? buf.readFloatLE(at) : buf.readInt16LE(at) / 32767;
  }
  return { sampleRate, samples };
}

/** Feed a recording through a detector in 2048 sample blocks, as the page does. */
function run(name, opts = {}) {
  const { sampleRate, samples } = readWav(name);
  const det = D.createDetector({ sampleRate, ...opts });
  const events = [];
  for (let i = 0; i < samples.length; i += 2048) events.push(...det.push(samples.subarray(i, i + 2048)));
  const of = (type) => events.filter((e) => e.type === type);
  return { det, of, readings: of("reading"), beats: of("beat") };
}

const last = (xs) => xs[xs.length - 1];

for (const [name, rate, bandLo, bandHi] of [
  // whole-recording rate, and the range 10 s readings stay in (the watch's
  // own rate wanders over seconds)
  ["watch-18000-quiet.wav", 76.8, 60, 105],
  ["watch-18000-noisy.wav", 98.7, 85, 110],
]) {
  test(`${name}: auto finds 18000 bph and reads the rate`, () => {
    const { of, beats, readings, det } = run(name, { average: 30 });
    assert.deepEqual(of("lock"), [{ type: "lock", bph: 18000, auto: true }]);
    // 5 beats/s for 30 s, all heard; the first 8 go to finding the rate
    assert.ok(beats.length >= 144 && beats.length <= 151, `${beats.length} onsets`);
    assert.ok(beats.filter((b) => b.n !== null).length >= 135);
    const r = last(readings);
    assert.ok(Math.abs(r.rate - rate) < 3, `rate ${r.rate}`);
    assert.ok(r.beatError > 3.5 && r.beatError < 4.5, `beat error ${r.beatError}`);
    assert.ok(det.levels().rejected <= 3);
  });

  test(`${name}: every sensitivity reads it`, () => {
    for (const sensitivity of Object.keys(D.SENSITIVITY)) {
      const { of, readings } = run(name, { average: 30, sensitivity });
      assert.deepEqual(of("lock").map((l) => l.bph), [18000], sensitivity);
      assert.ok(Math.abs(last(readings).rate - rate) < 3, `${sensitivity}: rate ${last(readings).rate}`);
    }
  });

  test(`${name}: 10 s readings stay steady`, () => {
    const { readings } = run(name, { bph: 18000, average: 10 });
    const settled = readings.filter((r) => r.settled);
    assert.ok(settled.length > 80, `${settled.length} settled readings`);
    for (const r of settled) assert.ok(r.rate > bandLo && r.rate < bandHi, `rate ${r.rate} at ${r.t}`);
  });
}
