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

test("readings say how much of the averaging window they cover", () => {
  const { of } = run({ bph: 28800, rate: 5 }, { bph: 28800, average: 10 }, 14);
  const readings = of("reading");
  const first = readings[0];
  assert.equal(first.window, 10);
  assert.equal(first.settled, false);
  assert.ok(first.span >= 1 && first.span < 1.2, `first span ${first.span}`);
  // spans grow until the window fills, then hold just under it
  const settledAt = readings.findIndex((r) => r.settled);
  assert.ok(settledAt > 0);
  // the first beat past 10 s less two periods
  const s = readings[settledAt].span;
  assert.ok(s >= 9.75 && s < 9.9, `${s}`);
  assert.ok(readings.slice(settledAt).every((r) => r.settled && r.span < 10));
  for (let i = 1; i < settledAt; i++) assert.ok(readings[i].span > readings[i - 1].span);
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

test("auto doubts a rate that stops fitting until it relocks (TICK-37)", () => {
  const det = D.createDetector({ sampleRate: SR });
  const a = D.synth({ sampleRate: SR, bph: 28800 });
  for (let i = 0; i < 6 * SR; i += 2048) det.push(a.next(2048));
  assert.equal(det.doubt(), false);
  const b = D.synth({ sampleRate: SR, bph: 18000, rate: 4, start: 0.3 });
  let doubted = false;
  let relocked = false;
  for (let i = 0; i < 12 * SR && !relocked; i += 2048) {
    relocked = det.push(b.next(2048)).some((e) => e.type === "lock");
    if (det.doubt()) doubted = true;
  }
  assert.ok(doubted, "doubted before relocking");
  assert.ok(relocked);
  assert.equal(det.doubt(), false);
});

test("holding auto's rate keeps its beats and stops auto relocking (TICK-37)", () => {
  const det = D.createDetector({ sampleRate: SR });
  const a = D.synth({ sampleRate: SR, bph: 28800 });
  for (let i = 0; i < 6 * SR; i += 2048) det.push(a.next(2048));
  assert.deepEqual(det.setBph(28800), [{ type: "lock", bph: 28800, auto: false, kept: true }]);
  // readings go on with no new lock
  const more = [];
  for (let i = 0; i < SR; i += 2048) more.push(...det.push(a.next(2048)));
  assert.ok(more.some((e) => e.type === "reading"));
  assert.ok(!more.some((e) => e.type === "lock"));
  // a different watch no longer moves it, and held is never in doubt
  const b = D.synth({ sampleRate: SR, bph: 18000, start: 0.3 });
  const after = [];
  for (let i = 0; i < 12 * SR; i += 2048) after.push(...det.push(b.next(2048)));
  assert.ok(!after.some((e) => e.type === "lock"));
  assert.equal(det.doubt(), false);
  // another rate than auto's locks afresh
  const det2 = D.createDetector({ sampleRate: SR });
  for (let i = 0; i < 6 * SR; i += 2048) det2.push(a.next(2048));
  assert.deepEqual(det2.setBph(21600), [{ type: "lock", bph: 21600, auto: false }]);
});

/** Feed blocks from `parts` ([generator, secs] pairs) and collect events. */
function feedParts(det, parts) {
  const events = [];
  for (const [gen, secs] of parts) {
    for (let i = 0; i < secs * SR; i += 2048) events.push(...det.push(gen.next(2048)));
  }
  return events;
}

const zeros = { next: (n) => new Float32Array(n) };

test("quiet ticks a little over room noise are read", () => {
  // tick peaks 3x the noise amplitude (about 2.5x its envelope)
  const { of } = run({ bph: 28800, rate: 4, beatError: 0.5, amp: 0.012, noise: 0.004, seed: 5 }, { average: 30 }, 40);
  assert.deepEqual(of("lock").map((e) => e.bph), [28800]);
  const r = last(of("reading"));
  assert.ok(Math.abs(r.rate - 4) < 1, `rate ${r.rate}`);
  assert.ok(Math.abs(r.beatError - 0.5) < 0.3, `beat error ${r.beatError}`);
});

test("higher sensitivity reads ticks lower in the noise", () => {
  const watch = { bph: 28800, rate: -6, amp: 0.008, noise: 0.004, seed: 9 };
  const high = run(watch, { sensitivity: "high", average: 30 }, 40);
  assert.deepEqual(high.of("lock").map((e) => e.bph), [28800]);
  assert.ok(Math.abs(last(high.of("reading")).rate + 6) < 2, `rate ${last(high.of("reading")).rate}`);
  // low sensitivity hears nothing in the same signal
  assert.equal(run(watch, { sensitivity: "low" }, 10).of("beat").length, 0);
});

test("normal sensitivity does not trigger on plain room noise", () => {
  for (const sensitivity of ["low", "normal", "high"]) {
    const { of } = run({ amp: 0, noise: 0.004, seed: 2 }, { sensitivity }, 20);
    assert.equal(of("beat").length, 0, sensitivity);
  }
});

test("silence at the start of the stream does not deafen the detector", () => {
  const det = D.createDetector({ sampleRate: SR });
  const watch = D.synth({ sampleRate: SR, bph: 28800, rate: 3, amp: 0.04, noise: 0.004 });
  const events = feedParts(det, [
    [zeros, 0.5],
    [watch, 12],
  ]);
  const beats = events.filter((e) => e.type === "beat");
  assert.ok(beats.length > 80, `${beats.length} beats`);
  assert.ok(Math.abs(last(events.filter((e) => e.type === "reading")).rate - 3) < 0.5);
});

test("loud knocks are ignored and do not hide the ticks after them", () => {
  const det = D.createDetector({ sampleRate: SR });
  const watch = D.synth({ sampleRate: SR, bph: 21600, rate: 9, amp: 0.01, noise: 0.002 });
  const knock = D.synth({ sampleRate: SR, bph: 12000, start: 0, noise: 0, amp: 0.6 }).next(1200);
  const events = [];
  let at = 0;
  for (let i = 0; i < 20 * SR; i += 2048, at += 2048) {
    const block = watch.next(2048);
    // a knock 50x the ticks every 1.7 s, between beats
    for (let j = 0; j < 2048; j++) {
      const off = (at + j) % Math.round(1.7 * SR) - Math.round(0.07 * SR);
      if (at + j > 3 * SR && off >= 0 && off < knock.length) block[j] += knock[off];
    }
    events.push(...det.push(block));
  }
  assert.deepEqual(events.filter((e) => e.type === "lock").map((e) => e.bph), [21600]);
  const beats = events.filter((e) => e.type === "beat");
  assert.ok(beats.every((b) => b.peak < 0.05), "no knock counted as a beat");
  // 6 beats/s for ~19.8 s, nearly all heard
  assert.ok(beats.length > 110, `${beats.length} beats`);
  assert.ok(Math.abs(last(events.filter((e) => e.type === "reading")).rate - 9) < 0.5);
  assert.ok(det.levels().rejected > 5, "knocks counted as rejected");
});

test("a watch moved closer to the mic is followed, not rejected as knocks", () => {
  const det = D.createDetector({ sampleRate: SR });
  const far = D.synth({ sampleRate: SR, bph: 28800, rate: -2, amp: 0.02, noise: 0.002 });
  const near = D.synth({ sampleRate: SR, bph: 28800, rate: -2, amp: 0.3, noise: 0.002, start: 0.05 });
  const events = feedParts(det, [
    [far, 6],
    [near, 10],
  ]);
  const late = events.filter((e) => e.type === "beat" && e.t > 8);
  // 8 beats/s over the last ~8 s, all heard once the loud level is accepted
  assert.ok(late.length > 60, `${late.length} beats after moving closer`);
  assert.ok(det.levels().rejected <= 6);
  assert.ok(Math.abs(last(events.filter((e) => e.type === "reading")).rate + 2) < 0.5);
});

test("a louder room raises the noise floor within a second", () => {
  const det = D.createDetector({ sampleRate: SR });
  const quiet = D.synth({ sampleRate: SR, amp: 0, noise: 0.001 });
  const loud = D.synth({ sampleRate: SR, amp: 0, noise: 0.01, seed: 4 });
  feedParts(det, [[quiet, 2]]);
  const before = det.levels().noise;
  feedParts(det, [[loud, 1.5]]);
  const after = det.levels();
  assert.ok(after.noise > 5 * before, `${before} -> ${after.noise}`);
  assert.ok(after.threshold > after.noise);
  assert.ok(after.level > 0);
  // level is the peak since the last call
  assert.equal(det.levels().level, 0);
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

/**
 * 20 s of an 18000 bph watch at +100 s/d with clicks about 12 a second at
 * random, each at `lo` to `hi` of the ticks' level.
 */
function clicky(detOpts, lo, hi) {
  const det = D.createDetector({ sampleRate: SR, ...detOpts });
  const watch = D.synth({ sampleRate: SR, bph: 18000, rate: 100, amp: 0.05, noise: 0.002 });
  const click = D.synth({ sampleRate: SR, bph: 12000, start: 0, noise: 0, amp: 0.05 }).next(600);
  let seed = 5;
  const rand = () => ((seed = (seed * 16807) % 2147483647) / 2147483647);
  const events = [];
  let nextClick = Math.round(0.5 * SR);
  let gain = lo;
  for (let at = 0; at < 20 * SR; at += 2048) {
    const block = watch.next(2048);
    for (let j = 0; j < 2048; j++) {
      const off = at + j - nextClick;
      if (off >= 0 && off < click.length) block[j] += gain * click[off];
      if (off === click.length) {
        nextClick = at + j + Math.round((0.02 + 0.14 * rand()) * SR);
        gain = lo + (hi - lo) * rand();
      }
    }
    events.push(...det.push(block));
  }
  return { det, events };
}

test("quieter clicks between ticks don't make the ticks look like knocks", () => {
  // auto: the lock clears the beat peaks, and the clicks come in first
  const { det, events } = clicky({}, 0.1, 0.2);
  assert.ok(det.levels().rejected <= 2, `${det.levels().rejected} ticks ignored as loud`);
  const indexed = events.filter((e) => e.type === "beat" && e.n !== null && !e.replaced);
  // 5 beats/s for the ~18 s after the lock, nearly all kept
  assert.ok(indexed.length >= 88, `${indexed.length} beats`);
  const r = last(events.filter((e) => e.type === "reading"));
  assert.ok(Math.abs(r.rate - 100) < 1, `rate ${r.rate}`);
});

test("a fit that indexed clicks as beats starts over instead of reading thousands of s/d", () => {
  // clicks up to half the ticks' level get some onsets indexed as beats
  const { events } = clicky({ bph: 18000 }, 0.1, 0.5);
  const readings = events.filter((e) => e.type === "reading");
  for (const r of readings) assert.ok(Math.abs(r.rate) <= 1500, `rate ${r.rate} at ${r.t}`);
});

test("a stray click before a beat does not hold off the beat", () => {
  const det = D.createDetector({ sampleRate: SR });
  const bph = 18000;
  const rate = 100;
  const watch = D.synth({ sampleRate: SR, bph, rate, amp: 0.05, noise: 0.002 });
  const click = D.synth({ sampleRate: SR, bph: 12000, start: 0, noise: 0, amp: 0.05 }).next(1200);
  const period = D.periodOf(bph) / (1 + rate / 86400);
  // a click as loud as the ticks, 0.65 of a period after every 7th beat from 4 s on
  const clickAt = new Set();
  for (let k = 0; 0.15 + k * period < 20; k++) {
    if (k % 7 === 0 && 0.15 + k * period > 4) clickAt.add(Math.round((0.15 + (k + 0.65) * period) * SR));
  }
  const events = [];
  let at = 0;
  for (let i = 0; i < 20 * SR; i += 2048, at += 2048) {
    const block = watch.next(2048);
    for (const c of clickAt) {
      for (let j = Math.max(0, c - at); j < 2048 && c - at + click.length > j; j++) block[j] += click[at + j - c];
    }
    events.push(...det.push(block));
  }
  assert.deepEqual(events.filter((e) => e.type === "lock").map((e) => e.bph), [bph]);
  const beats = events.filter((e) => e.type === "beat" && e.n !== null);
  assert.ok(beats.some((b) => b.replaced), "a click was replaced by the beat after it");
  // the beat each index ends up with is the real one, and none are lost
  const kept = new Map(beats.map((b) => [b.n, b.t]));
  const late = [...kept].filter(([, t]) => t > 5 && t < 19.9);
  assert.ok(late.length >= Math.floor(14.9 / period) - 1, `${late.length} beats kept`);
  for (const [, t] of late) {
    const phase = (t - 0.15) / period;
    assert.ok(Math.abs(phase - Math.round(phase)) < 0.05, `beat at ${t} is off phase`);
  }
  assert.ok(Math.abs(last(events.filter((e) => e.type === "reading")).rate - rate) < 0.5);
});

test("robustFit refits until the outliers stop changing", () => {
  const beats = [];
  for (let n = 0; n < 40; n++) beats.push({ t: (n * 0.2) / (1 + 100 / 86400), n });
  // a beat caught way off, which hides a smaller miss on the first pass
  beats[5].t += 0.05;
  beats[30].t += 0.004;
  const rate = (f) => 86400 * (0.2 / f.b - 1);
  const f = D.robustFit(beats);
  assert.ok(Math.abs(rate(f) - 100) < 1e-6, `robust fit ${rate(f)}`);
});

test("analyze fits the longest unbroken run of beats", () => {
  const gen = D.synth({ sampleRate: SR, bph: 21600, rate: -8, beatError: 0.6 });
  const quiet = D.synth({ sampleRate: SR, amp: 0, noise: 0.002, seed: 9 });
  // 3 s of beats, a 4 s silence that restarts indexing, then 8 s more
  const parts = [gen.next(3 * SR), quiet.next(4 * SR), gen.next(8 * SR)];
  const samples = new Float32Array(15 * SR);
  parts.reduce((at, p) => (samples.set(p, at), at + p.length), 0);
  const { summary, duration, events } = D.analyze(samples, { sampleRate: SR, bph: 21600, average: 4 });
  assert.equal(duration, 15);
  assert.equal(summary.bph, 21600);
  assert.equal(summary.auto, false);
  assert.ok(Math.abs(summary.rate + 8) < 0.3, `rate ${summary.rate}`);
  assert.ok(Math.abs(summary.beatError - 0.6) < 0.05);
  // the 8 s run, not the 3 s one
  assert.ok(summary.span > 7 && summary.span < 8, `span ${summary.span}`);
  assert.ok(events.filter((e) => e.type === "beat").some((b) => b.n === 0 && b.t > 7));

  // nothing to read
  const none = D.analyze(new Float32Array(SR), { sampleRate: SR });
  assert.deepEqual(none, { events: [], duration: 1, summary: null });
  assert.equal(D.analyze(new Float32Array(48000)).duration, 1);
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
  // a few noise onsets among the beats don't sink the true rate
  const q = D.periodOf(18000);
  const noisy = [...Array(20).fill(q), 0.35 * q, 0.6 * q, 0.45 * q, 0.7 * q];
  assert.equal(D.bestBph(noisy).bph, 18000);
  assert.ok(D.scorePeriod(q, noisy) <= 0.08, `score ${D.scorePeriod(q, noisy)}`);
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
