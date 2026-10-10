// Detector accuracy against synthesized watches with known rate and beat
// error (spec/tick.md TICK-7 to TICK-13).
import { test } from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const D = require("../../assets/static/detector.js");

const SR = 48000;

/** Run `secs` of a synthesized watch through a detector in 2048 blocks. */
function run(watch, detOpts = {}, secs = 20, sampleRate = SR) {
  const gen = D.synth({ sampleRate, ...watch });
  const det = D.createDetector({ sampleRate, ...detOpts });
  const events = [];
  let left = Math.round(secs * sampleRate);
  while (left > 0) {
    const n = Math.min(2048, left);
    events.push(...det.push(gen.next(n)));
    left -= n;
  }
  return { det, events, of: (type) => events.filter((e) => e.type === type) };
}

const last = (xs) => xs[xs.length - 1];

for (const [bph, rate, beatError] of [
  [28800, 6, 0.4],
  [28800, -12.5, 0],
  [21600, 30, 1.2],
  [18000, -3, 0.8],
  [36000, 0, 0.2],
]) {
  test(`reads ${bph} bph at ${rate} s/d with ${beatError} ms beat error`, () => {
    const { of } = run({ bph, rate, beatError });
    const locks = of("lock");
    assert.equal(locks.length, 1, "locks once");
    assert.deepEqual(locks[0], { type: "lock", bph, auto: true });
    const r = last(of("reading"));
    assert.ok(Math.abs(r.rate - rate) < 0.3, `rate ${r.rate}`);
    assert.ok(Math.abs(r.beatError - beatError) < 0.05, `beat error ${r.beatError}`);
    assert.equal(r.bph, bph);
  });
}

test("each beat is counted once and indexed in order", () => {
  const { of } = run({ bph: 28800, rate: 6 }, { bph: 28800 }, 5);
  const beats = of("beat");
  // 5 s at 8 beats/s, less the first beat before 0.15 s and the last 30 ms
  assert.ok(beats.length >= 38 && beats.length <= 40, `${beats.length} beats`);
  beats.forEach((b, i) => {
    assert.equal(b.n, i);
    assert.equal(b.parity, i % 2);
    assert.equal(b.wave.length, Math.round(0.032 * SR));
    assert.ok(b.peak > 0);
  });
});

test("a fast watch lands its beats early, so trace offsets climb", () => {
  const { of } = run({ bph: 28800, rate: 60 }, { bph: 28800 }, 10);
  const beats = of("beat");
  const first = beats[2].offsetMs;
  const lastOffset = last(beats).offsetMs;
  // 60 s/d is ~0.69 ms per second
  assert.ok(lastOffset - first > 5, `${first} -> ${lastOffset}`);
});

test("beat error separates even and odd offsets", () => {
  const { of } = run({ bph: 28800, beatError: 2 }, { bph: 28800 }, 5);
  const beats = of("beat").slice(4);
  const even = beats.filter((b) => b.parity === 0).map((b) => b.offsetMs);
  const odd = beats.filter((b) => b.parity === 1).map((b) => b.offsetMs);
  const mean = (xs) => xs.reduce((a, b) => a + b, 0) / xs.length;
  assert.ok(Math.abs(mean(even) - mean(odd) - 2) < 0.05);
});

test("a fixed beat rate locks before any beat", () => {
  const det = D.createDetector({ sampleRate: SR, bph: 21600 });
  // the lock from creation is not replayed into the first push
  assert.deepEqual(det.push(new Float32Array(100)), []);
  const { of } = run({ bph: 21600, rate: 10 }, { bph: 21600 }, 10);
  assert.equal(of("lock").length, 0);
  assert.ok(Math.abs(last(of("reading")).rate - 10) < 0.3);
});

test("beats before the rate is known carry no index", () => {
  const { of } = run({ bph: 28800 }, {}, 0.6);
  const beats = of("beat");
  assert.ok(beats.length >= 3);
  for (const b of beats) {
    assert.equal(b.n, null);
    assert.equal(b.offsetMs, null);
  }
  assert.equal(of("reading").length, 0);
});

test("missed beats keep their index and the reading", () => {
  const { of } = run({ bph: 28800, rate: -8, drop: 0.2, seed: 7 }, {}, 20);
  const beats = of("beat").filter((b) => b.n !== null);
  const gaps = beats.slice(1).filter((b, i) => b.n - beats[i].n > 1);
  assert.ok(gaps.length > 5, "some beats were skipped");
  assert.ok(Math.abs(last(of("reading")).rate + 8) < 0.5);
});

test("mic correction is added to the rate", () => {
  const plain = last(run({ bph: 28800, rate: 5 }, { bph: 28800 }, 8).of("reading"));
  const corrected = last(run({ bph: 28800, rate: 5 }, { bph: 28800, correction: -2.5 }, 8).of("reading"));
  assert.ok(Math.abs(corrected.rate - (plain.rate - 2.5)) < 1e-9);
});

test("setters change the beat rate, averaging, and correction live", () => {
  const gen = D.synth({ sampleRate: SR, bph: 28800, rate: 5 });
  const det = D.createDetector({ sampleRate: SR });
  const feed = (secs) => {
    const out = [];
    for (let i = 0; i < secs * SR; i += 2048) out.push(...det.push(gen.next(2048)));
    return out;
  };
  feed(5);
  assert.deepEqual(det.setBph(18000), [{ type: "lock", bph: 18000, auto: false }]);
  det.setBph(28800);
  det.setAverage(4);
  det.setCorrection(1);
  const readings = feed(6).filter((e) => e.type === "reading");
  assert.ok(Math.abs(last(readings).rate - 6) < 0.3);
  assert.deepEqual(det.setBph("auto"), []);
  const relocked = feed(4);
  assert.deepEqual(relocked.find((e) => e.type === "lock"), { type: "lock", bph: 28800, auto: true });
  // whole 2048 sample blocks run a little past each feed
  assert.ok(Math.abs(det.time() - 15.0) < 0.2);
  assert.deepEqual(det.waveMs, { pre: 2, post: 30 });
});

test("auto relocks when the watch changes beat rate", () => {
  const det = D.createDetector({ sampleRate: SR });
  const a = D.synth({ sampleRate: SR, bph: 28800 });
  const events = [];
  for (let i = 0; i < 6 * SR; i += 2048) events.push(...det.push(a.next(2048)));
  // a different watch after a short pause
  const b = D.synth({ sampleRate: SR, bph: 18000, rate: 4, start: 0.3 });
  for (let i = 0; i < 12 * SR; i += 2048) events.push(...det.push(b.next(2048)));
  const locks = events.filter((e) => e.type === "lock").map((e) => e.bph);
  assert.deepEqual(locks, [28800, 18000]);
  assert.ok(Math.abs(last(events.filter((e) => e.type === "reading")).rate - 4) < 0.5);
});

test("a burst of stray clicks does not relock the beat rate", () => {
  const det = D.createDetector({ sampleRate: SR });
  const watch = D.synth({ sampleRate: SR, bph: 28800, rate: 7 });
  const click = D.synth({ sampleRate: SR, bph: 12000, start: 0, noise: 0, amp: 0.8 }).next(1200);
  let at = 0;
  let nextClick = 0;
  let seed = 3;
  const rand = () => ((seed = (seed * 16807) % 2147483647) / 2147483647);
  const events = [];
  const feed = (secs, clicks) => {
    for (let i = 0; i < secs * SR; i += 2048, at += 2048) {
      const block = watch.next(2048);
      if (clicks) {
        for (let j = 0; j < 2048; j++) {
          if (at + j >= nextClick) nextClick = at + j + Math.round((0.03 + 0.05 * rand()) * SR);
          const off = at + j - (nextClick - Math.round(0.03 * SR));
          if (off >= 0 && off < click.length) block[j] += click[off];
        }
      }
      events.push(...det.push(block));
    }
  };
  feed(5, false);
  feed(4, true);
  feed(12, false);
  assert.deepEqual(events.filter((e) => e.type === "lock").map((e) => e.bph), [28800]);
  assert.ok(Math.abs(last(events.filter((e) => e.type === "reading")).rate - 7) < 0.3);
});

test("robustFit leaves a beat caught late out of the fit", () => {
  const beats = [];
  for (let n = 0; n < 40; n++) beats.push({ t: (n * 0.125) / (1 + 5 / 86400) + (n % 2 ? 0.0003 : 0), n });
  // caught on its drop sound, 9 ms late
  beats[17].t += 0.009;
  const plain = D.fit(beats);
  const robust = D.robustFit(beats);
  const rate = (f) => 86400 * (0.125 / f.b - 1);
  assert.ok(Math.abs(rate(plain) - 5) > 1, `plain fit ${rate(plain)}`);
  assert.ok(Math.abs(rate(robust) - 5) < 1e-6, `robust fit ${rate(robust)}`);
  assert.ok(Math.abs(2 * Math.abs(robust.h) - 0.0003) < 1e-9);
  // nothing to drop: the plain fit comes back
  beats[17].t -= 0.009;
  assert.deepEqual(D.robustFit(beats), D.fit(beats));
  // too few left after dropping: keep the plain fit
  const few = beats.slice(0, 6);
  few[2].t += 0.009;
  assert.deepEqual(D.robustFit(few), D.fit(few));
  assert.equal(D.robustFit([{ t: 0, n: 0 }]), null);
});

test("a long silence restarts the fit", () => {
  const det = D.createDetector({ sampleRate: SR, bph: 28800 });
  const a = D.synth({ sampleRate: SR, bph: 28800, rate: 20 });
  const quiet = D.synth({ sampleRate: SR, amp: 0, noise: 0.002 });
  const events = [];
  for (let i = 0; i < 4 * SR; i += 2048) events.push(...det.push(a.next(2048)));
  for (let i = 0; i < 4 * SR; i += 2048) events.push(...det.push(quiet.next(2048)));
  for (let i = 0; i < 4 * SR; i += 2048) events.push(...det.push(a.next(2048)));
  const beats = events.filter((e) => e.type === "beat");
  const restart = beats.findIndex((b, i) => i > 0 && b.n === 0);
  assert.ok(restart > 0, "indexing restarts after the silence");
  assert.ok(beats[restart].t - beats[restart - 1].t > 3);
});

test("silence and noise alone produce no beats", () => {
  const silent = run({ amp: 0, noise: 0 }, {}, 2);
  assert.equal(silent.events.length, 0);
  const hiss = run({ amp: 0, noise: 0.01 }, {}, 3);
  assert.equal(hiss.of("lock").length, 0);
  assert.equal(hiss.of("reading").length, 0);
});

test("works at 44.1 kHz", () => {
  const { of } = run({ bph: 28800, rate: -4, beatError: 0.5, sampleRate: 44100 }, {}, 15, 44100);
  const r = last(of("reading"));
  assert.ok(Math.abs(r.rate + 4) < 0.3, `rate ${r.rate}`);
  assert.ok(Math.abs(r.beatError - 0.5) < 0.05);
});

test("scorePeriod prefers the true period and tolerates skipped beats", () => {
  const p = D.periodOf(28800);
  const intervals = [p, p, 2 * p, p, p, p, 3 * p, p];
  assert.equal(D.bestBph(intervals).bph, 28800);
  assert.ok(D.scorePeriod(p, intervals) < 0.02);
  assert.ok(D.scorePeriod(D.periodOf(21600), intervals) > 0.1);
  // an interval shorter than half a period fits nothing
  assert.equal(D.scorePeriod(p, [p / 3]), 1);
});

test("fit recovers slope and parity offset, and gives up on degenerate input", () => {
  const beats = [];
  for (let n = 0; n < 20; n++) beats.push({ t: 1 + n * 0.125 + (n % 2 ? 0.0004 : 0), n });
  const f = D.fit(beats);
  assert.ok(Math.abs(f.b - 0.125) < 1e-12);
  assert.ok(Math.abs(2 * Math.abs(f.h) - 0.0004) < 1e-12);
  assert.equal(D.fit([{ t: 0, n: 0 }, { t: 0.25, n: 2 }, { t: 0.5, n: 4 }]), null);
});

test("highpass blocks dc", () => {
  const hp = D.highpass(SR, 1000);
  let y = 0;
  for (let i = 0; i < SR / 10; i++) y = hp(1);
  assert.ok(Math.abs(y) < 1e-6);
});

test("synth defaults and determinism", () => {
  const a = D.synth().next(4800);
  const b = D.synth().next(4800);
  assert.deepEqual(a, b);
  assert.equal(D.synth().sampleRate, 48000);
  assert.notDeepEqual(D.synth({ seed: 2 }).next(100), D.synth({ seed: 3 }).next(100));
  // defaults: a detector with defaults reads the default watch
  const det = D.createDetector();
  const gen = D.synth();
  let r = null;
  for (let i = 0; i < 6 * 48000; i += 2048) {
    for (const e of det.push(gen.next(2048))) if (e.type === "reading") r = e;
  }
  assert.equal(r.bph, 28800);
  assert.ok(Math.abs(r.rate) < 0.3);
});
