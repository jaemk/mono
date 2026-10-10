// tick detector: beat onsets, beat rate, and the rate and beat error fit over
// raw mono samples (spec/tick.md TICK-7 to TICK-13). It has no DOM, so the
// node tests drive it with `synth` audio and the page feeds it the
// microphone or the simulator.
(function (root) {
  "use strict";

  const BPH = [12000, 14400, 18000, 19800, 21600, 25200, 28800, 36000];
  const HP_HZ = 1000;
  const ENV_TAU = 0.0003;
  // the noise floor averages the envelope: quickly while the filters settle,
  // then only slowly while the envelope is over the threshold, so beats
  // barely lift it
  const NOISE_WARM_TAU = 0.02;
  const NOISE_QUIET_TAU = 0.3;
  const NOISE_LOUD_TAU = 5;
  // the recent beat peak fades so a quieter watch is picked up again
  const PEAK_TAU = 2;
  const FLOOR = 1e-4;
  const NOISE_MULT = 3;
  const PEAK_FRAC = 0.25;
  // filter settle time before onsets count
  const WARMUP = 0.1;
  const PRE = 0.002;
  const POST = 0.03;
  const MIN_GAP = 0.05;
  const GAP_FRAC = 0.6;
  const AUTO_MIN = 8;
  const AUTO_INTERVALS = 24;
  const AUTO_FIT = 0.08;
  const MULTI_PENALTY = 0.02;
  const MIN_READ_BEATS = 6;
  const MIN_READ_SPAN = 1;
  // a beat this far off the fit (and 5x the median miss) is left out of it
  const OUTLIER_MIN = 0.0005;
  const OUTLIER_MULT = 5;
  // a silence this long restarts the fit instead of bridging it
  const MAX_SILENCE = 3;

  const periodOf = (bph) => 3600 / bph;

  /** RBJ biquad high-pass, direct form I. */
  function highpass(sampleRate, hz) {
    const w0 = (2 * Math.PI * hz) / sampleRate;
    const cos = Math.cos(w0);
    const alpha = Math.sin(w0) / (2 * Math.SQRT1_2);
    const a0 = 1 + alpha;
    const b0 = (1 + cos) / 2 / a0;
    const b1 = -(1 + cos) / a0;
    const b2 = b0;
    const a1 = (-2 * cos) / a0;
    const a2 = (1 - alpha) / a0;
    let x1 = 0;
    let x2 = 0;
    let y1 = 0;
    let y2 = 0;
    return (x) => {
      const y = b0 * x + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
      x2 = x1;
      x1 = x;
      y2 = y1;
      y1 = y;
      return y;
    };
  }

  /**
   * How well a nominal period explains onset intervals: the mean distance
   * from each interval to its nearest whole number of periods, as a fraction
   * of the period, plus a small penalty per interval that spans missed beats.
   */
  function scorePeriod(period, intervals) {
    let total = 0;
    for (const d of intervals) {
      const k = Math.round(d / period);
      total += k < 1 ? 1 : Math.abs(d - k * period) / period + (k > 1 ? MULTI_PENALTY : 0);
    }
    return total / intervals.length;
  }

  /** The best fitting standard beat rate for the intervals. */
  function bestBph(intervals) {
    let best = null;
    for (const bph of BPH) {
      const score = scorePeriod(periodOf(bph), intervals);
      if (!best || score < best.score) best = { bph, score };
    }
    return best;
  }

  /**
   * Least squares fit of `t = a + b*n + h*s` (s = +1 even, -1 odd), centered
   * for precision. Returns `{ b, h, residual }` (each beat's miss in s) or
   * null when the beats can't separate the terms.
   */
  function fit(beats) {
    const t0 = beats[0].t;
    const n0 = beats[0].n;
    let sn = 0, ss = 0, snn = 0, sns = 0, sss = 0, st = 0, snt = 0, sst = 0;
    for (const { t, n } of beats) {
      const x = n - n0;
      const s = n % 2 === 0 ? 1 : -1;
      const y = t - t0;
      sn += x;
      ss += s;
      snn += x * x;
      sns += x * s;
      sss += s * s;
      st += y;
      snt += x * y;
      sst += s * y;
    }
    const m = beats.length;
    // normal equations: [[m, sn, ss], [sn, snn, sns], [ss, sns, sss]] * [a, b, h] = [st, snt, sst]
    const det3 = (a, b, c, d, e, f, g, h, i) => a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    const det = det3(m, sn, ss, sn, snn, sns, ss, sns, sss);
    if (Math.abs(det) < 1e-9) return null;
    const a = det3(st, sn, ss, snt, snn, sns, sst, sns, sss) / det;
    const b = det3(m, st, ss, sn, snt, sns, ss, sst, sss) / det;
    const h = det3(m, sn, st, sn, snn, snt, ss, sns, sst) / det;
    const residual = beats.map(({ t, n }) => t - t0 - (a + b * (n - n0) + h * (n % 2 === 0 ? 1 : -1)));
    return { b, h, residual };
  }

  /** `fit`, refit once without the beats that miss it by far (TICK-12). */
  function robustFit(beats) {
    const f = fit(beats);
    if (!f) return null;
    const miss = f.residual.map(Math.abs);
    const sorted = [...miss].sort((x, y) => x - y);
    const limit = Math.max(OUTLIER_MIN, OUTLIER_MULT * sorted[sorted.length >> 1]);
    const kept = beats.filter((_, i) => miss[i] <= limit);
    if (kept.length === beats.length || kept.length < MIN_READ_BEATS) return f;
    return fit(kept) || f;
  }

  /**
   * A streaming detector. `push(samples)` takes the next block of samples and
   * returns the events it produced, in order:
   *
   * - `{ type: "lock", bph, auto }`: the beat rate was chosen (or changed);
   *   earlier beats no longer count
   * - `{ type: "beat", t, n, parity, offsetMs, peak, wave }`: one beat; `n`,
   *   `parity`, and `offsetMs` are null until the beat rate is known
   * - `{ type: "reading", t, rate, beatError, bph }`: rate in s/d (fast is
   *   positive) and beat error in ms, after each beat once enough are in
   */
  function createDetector(opts = {}) {
    const sr = opts.sampleRate || 48000;
    let bphSetting = opts.bph || "auto";
    let average = opts.average || 10;
    let correction = opts.correction || 0;

    const hp = highpass(sr, HP_HZ);
    const envA = Math.exp(-1 / (ENV_TAU * sr));
    const warmA = Math.exp(-1 / (NOISE_WARM_TAU * sr));
    const quietA = Math.exp(-1 / (NOISE_QUIET_TAU * sr));
    const loudA = Math.exp(-1 / (NOISE_LOUD_TAU * sr));
    const peakA = Math.exp(-1 / (PEAK_TAU * sr));
    const preN = Math.round(PRE * sr);
    const postN = Math.round(POST * sr);
    let ringSize = 1;
    while (ringSize < (preN + postN) * 2) ringSize *= 2;
    const ring = new Float32Array(ringSize);
    const mask = ringSize - 1;
    const warmup = Math.round(WARMUP * sr);

    let pos = 0;
    let env = 0;
    let noise = null;
    let peak = 0;
    let armedAt = 0;
    let pending = [];

    let period = null;
    let auto = bphSetting === "auto";
    let doubt = false;
    let intervals = [];
    let lastOnset = null;
    let beats = [];
    let prev = null;
    let t0 = null;
    let events = [];

    function lock(bph, isAuto) {
      period = periodOf(bph);
      auto = isAuto;
      doubt = false;
      resetFit();
      events.push({ type: "lock", bph, auto: isAuto });
    }

    function resetFit() {
      beats = [];
      prev = null;
      t0 = null;
    }

    // Lock the best fitting rate once one fits. Once locked, a full window the
    // locked rate no longer fits is dropped (it may straddle two watches) and
    // the choice is made again on fresh intervals.
    function chooseRate() {
      if (period !== null && !doubt) {
        if (intervals.length >= AUTO_INTERVALS && scorePeriod(period, intervals) > AUTO_FIT) {
          doubt = true;
          intervals = [];
        }
        return;
      }
      if (intervals.length < AUTO_MIN) return;
      if (period !== null && scorePeriod(period, intervals) <= AUTO_FIT) {
        doubt = false;
        return;
      }
      const best = bestBph(intervals);
      if (best.score <= AUTO_FIT) lock(best.bph, true);
    }

    function onBeat(t, peakAmp, wave) {
      if (lastOnset !== null) {
        intervals.push(t - lastOnset);
        if (intervals.length > AUTO_INTERVALS) intervals.shift();
      }
      lastOnset = t;
      if (auto) chooseRate();
      const beat = { type: "beat", t, n: null, parity: null, offsetMs: null, peak: peakAmp, wave };
      if (period === null) {
        events.push(beat);
        return;
      }
      if (prev && t - prev.t > MAX_SILENCE) resetFit();
      let n = 0;
      if (prev) {
        const k = Math.round((t - prev.t) / period);
        if (k < 1) return; // a second onset inside one beat: noise
        n = prev.n + k;
      } else {
        t0 = t;
      }
      prev = { t, n };
      beat.n = n;
      beat.parity = n % 2;
      // how early the beat landed, so a fast watch climbs
      beat.offsetMs = (t0 + n * period - t) * 1000;
      events.push(beat);

      beats.push(prev);
      while (beats.length && beats[0].t < t - average) beats.shift();
      const reading = readingAt(t);
      if (reading) events.push(reading);
    }

    function readingAt(t) {
      if (beats.length < MIN_READ_BEATS) return null;
      if (beats[beats.length - 1].t - beats[0].t < MIN_READ_SPAN) return null;
      const f = robustFit(beats);
      if (!f) return null;
      return {
        type: "reading",
        t,
        rate: 86400 * (period / f.b - 1) + correction,
        beatError: 2 * Math.abs(f.h) * 1000,
        bph: Math.round(3600 / period),
      };
    }

    function step(x) {
      const y = hp(x);
      ring[pos & mask] = y;
      const prevEnv = env;
      env = envA * env + (1 - envA) * Math.abs(y);
      if (noise === null) noise = env;
      peak *= peakA;

      for (const p of pending) if (env > p.envPeak) p.envPeak = env;

      const threshold = Math.max(FLOOR, noise + Math.max(NOISE_MULT * noise, PEAK_FRAC * (peak - noise)));
      const na = pos < warmup ? warmA : env < threshold ? quietA : loudA;
      noise = na * noise + (1 - na) * env;
      if (pos >= warmup && pos >= armedAt && prevEnv < threshold && env >= threshold) {
        const frac = (threshold - prevEnv) / (env - prevEnv);
        const onset = pos - 1 + frac;
        const gap = period === null ? MIN_GAP : Math.max(MIN_GAP, GAP_FRAC * period);
        armedAt = pos + Math.round(gap * sr);
        pending.push({ onset, at: pos, envPeak: env });
      }

      if (pending.length && pos >= pending[0].at + postN) {
        const p = pending.shift();
        const wave = new Float32Array(preN + postN);
        const start = p.at - preN;
        for (let i = 0; i < wave.length; i++) wave[i] = ring[(start + i) & mask];
        peak = peak === 0 ? p.envPeak : 0.7 * peak + 0.3 * p.envPeak;
        onBeat(p.onset / sr, p.envPeak, wave);
      }
      pos++;
    }

    function push(samples) {
      events = [];
      for (let i = 0; i < samples.length; i++) step(samples[i]);
      return events;
    }

    function setBph(bph) {
      bphSetting = bph;
      intervals = [];
      lastOnset = null;
      if (bph === "auto") {
        period = null;
        auto = true;
        resetFit();
        return [];
      }
      events = [];
      lock(bph, false);
      return events;
    }

    if (!auto) lock(bphSetting, false);
    events = [];

    return {
      push,
      setBph,
      setAverage: (secs) => {
        average = secs;
      },
      setCorrection: (sd) => {
        correction = sd;
      },
      time: () => pos / sr,
      sampleRate: sr,
      waveMs: { pre: PRE * 1000, post: POST * 1000 },
    };
  }

  /** Seeded PRNG (mulberry32) in [0, 1). */
  function prng(seed) {
    let a = seed >>> 0;
    return () => {
      a = (a + 0x6d2b79f5) >>> 0;
      let t = a;
      t = Math.imul(t ^ (t >>> 15), t | 1);
      t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
      return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
  }

  // the unlock, impulse, and drop sounds of one beat: offset (s), level,
  // carrier (hz)
  const BURSTS = [
    [0, 0.6, 4500],
    [0.004, 0.8, 5200],
    [0.009, 1, 6000],
  ];
  const BURST_TAU = 0.0006;
  const BEAT_LEN = 0.025;

  /**
   * A synthesized watch (TICK-5): `next(count)` returns the next `count`
   * samples. `rate` is in s/d (fast positive), `beatError` in ms (odd beats
   * land late by it), `noise` is the white noise level, and `drop` is the
   * chance a beat is silent.
   */
  function synth(opts = {}) {
    const sr = opts.sampleRate || 48000;
    const bph = opts.bph || 28800;
    const rate = opts.rate || 0;
    const beatError = (opts.beatError || 0) / 1000;
    const noise = opts.noise === undefined ? 0.002 : opts.noise;
    const amp = opts.amp === undefined ? 0.5 : opts.amp;
    const start = opts.start === undefined ? 0.15 : opts.start;
    const drop = opts.drop || 0;
    const rand = prng(opts.seed || 1);
    const period = periodOf(bph) / (1 + rate / 86400);
    const silent = new Map();
    let pos = 0;

    const isSilent = (k) => {
      if (!silent.has(k)) silent.set(k, rand() < drop);
      return silent.get(k);
    };

    function next(count) {
      const out = new Float32Array(count);
      for (let i = 0; i < count; i++, pos++) {
        const t = pos / sr;
        let v = noise * (rand() * 2 - 1);
        const first = Math.max(0, Math.floor((t - start - BEAT_LEN - beatError) / period));
        const last = Math.floor((t - start) / period);
        for (let k = first; k <= last; k++) {
          const tk = start + k * period + (k % 2 ? beatError : 0);
          const dt = t - tk;
          if (dt < 0 || dt >= BEAT_LEN || (drop && isSilent(k))) continue;
          for (const [off, level, hz] of BURSTS) {
            const u = dt - off;
            if (u >= 0) v += amp * level * Math.sin(2 * Math.PI * hz * u) * Math.exp(-u / BURST_TAU);
          }
        }
        out[i] = v;
      }
      return out;
    }

    return { next, sampleRate: sr };
  }

  const api = { BPH, createDetector, synth, scorePeriod, bestBph, fit, robustFit, highpass, periodOf };
  if (typeof module === "object" && module.exports) {
    module.exports = api;
  } else {
    root.TickDetector = api;
  }
})(typeof window === "undefined" ? globalThis : window);
