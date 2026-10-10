// tick detector: beat onsets, beat rate, and the rate and beat error fit over
// raw mono samples (spec/tick.md TICK-7 to TICK-13). It has no DOM, so the
// node tests drive it with `synth` audio and the page feeds it the
// microphone or the simulator.
(function (root) {
  "use strict";

  const BPH = [12000, 14400, 18000, 19800, 21600, 25200, 28800, 36000];
  // ticks are light, high clicks; room rumble, voices, and knocks sit lower
  const HP_HZ = 2000;
  const ENV_TAU = 0.0003;
  // the noise floor averages the envelope: quickly while the filters settle,
  // then only slowly while the envelope is briefly over the threshold, so
  // beats barely lift it. Over the threshold for longer than any beat lasts
  // means the room got louder (or the stream started with silence), and the
  // floor catches up quickly.
  const NOISE_WARM_TAU = 0.02;
  const NOISE_QUIET_TAU = 0.3;
  const NOISE_LOUD_TAU = 5;
  const SUSTAINED = 0.06;
  // only keeps digital silence from triggering; mic levels with gain control
  // off can be far below 1e-4
  const FLOOR = 1e-7;
  // how far over the noise floor an onset must reach, as a multiple of it
  // (pure white noise starts to trigger near 0.5)
  const SENSITIVITY = { low: 1.5, normal: 0.8, high: 0.6, max: 0.45 };
  // the threshold also stays over a quarter of the median recent beat peak
  const PEAK_FRAC = 0.25;
  const PEAKS = 16;
  // an onset this many times the median beat peak is a knock, not a beat
  const LOUD_MULT = 4;
  const LOUD_MIN_BEATS = 4;
  const LOUD_STREAK = 6;
  // filter settle time before onsets count
  const WARMUP = 0.1;
  const PRE = 0.002;
  const POST = 0.03;
  const LOOKBACK = 0.012;
  // low enough that the unlock, the first and often weakest sound, clears it
  // every beat; a level near another sound's height flips between them
  const CFD_FRAC = 0.3;
  const MIN_GAP = 0.05;
  const GAP_FRAC = 0.6;
  // once tracking, triggers count only this close (in periods) to where the
  // next beat is due, until no beat has come for GATE_PERIODS
  const GATE = 0.15;
  const GATE_PERIODS = 4;
  // in auto, fewer than HIT_MIN of the beats due over the last HIT_BEATS
  // accepted ones means the gate holds the wrong rate
  const HIT_BEATS = 16;
  const HIT_MIN = 0.5;
  // the beat rate is chosen by folding the last AUTO_ONSETS onset times
  // (at least AUTO_MIN); it locks once LOCK_MIN of them line up within
  // CLUSTER_W of a period
  const AUTO_MIN = 10;
  const AUTO_ONSETS = 24;
  const CLUSTER_W = 0.08;
  const LOCK_MIN = 0.5;
  const TIE = 0.05;
  const STRETCH = [-0.004, -0.002, 0, 0.002, 0.004];
  const MIN_READ_BEATS = 6;
  const MIN_READ_SPAN = 1;
  // a reading is settled once its beats span the averaging window, less this
  // many periods
  const SETTLE_SLACK = 2;
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
   * How well a nominal period explains onset times: fold them by the period
   * and take the largest share that lands within CLUSTER_W of one phase,
   * over a few slightly stretched periods so a watch far off rate still
   * lines up. Beats line up whatever beats were missed; a ringing tail or
   * a stray click lands elsewhere and only dilutes the share.
   */
  function scorePeriod(period, times) {
    let best = 0;
    for (const s of STRETCH) {
      const p = period * (1 + s);
      const phase = times.map((t) => (t / p) % 1);
      for (const a of phase) {
        let count = 0;
        for (const b of phase) {
          const d = Math.abs(a - b);
          if (Math.min(d, 1 - d) <= CLUSTER_W) count++;
        }
        best = Math.max(best, count);
      }
    }
    return best / times.length;
  }

  /**
   * The best fitting standard beat rate for the onset times. A watch's beats
   * also line up at any multiple of its rate (an 18000 watch folds cleanly
   * at 36000), but not at a fraction of it (a 36000 watch splits in two at
   * 18000), so on a near tie the lower rate wins.
   */
  function bestBph(times) {
    let best = null;
    for (const bph of BPH) {
      const score = scorePeriod(periodOf(bph), times);
      if (!best || score > best.score + TIE) best = { bph, score };
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
   * - `{ type: "reading", t, rate, beatError, bph, span, window, settled }`:
   *   rate in s/d (fast is positive) and beat error in ms, after each beat
   *   once enough are in; `span` is the seconds of beats behind it and
   *   `settled` says they fill the averaging `window`
   */
  function createDetector(opts = {}) {
    const sr = opts.sampleRate || 48000;
    let bphSetting = opts.bph || "auto";
    let average = opts.average || 10;
    let correction = opts.correction || 0;
    let mult = SENSITIVITY[opts.sensitivity] || SENSITIVITY.normal;

    const hp = highpass(sr, HP_HZ);
    const envA = Math.exp(-1 / (ENV_TAU * sr));
    const warmA = Math.exp(-1 / (NOISE_WARM_TAU * sr));
    const quietA = Math.exp(-1 / (NOISE_QUIET_TAU * sr));
    const loudA = Math.exp(-1 / (NOISE_LOUD_TAU * sr));
    const sustainedN = Math.round(SUSTAINED * sr);
    const silenceN = Math.round(MAX_SILENCE * sr);
    const preN = Math.round(PRE * sr);
    const postN = Math.round(POST * sr);
    const lookN = Math.round(LOOKBACK * sr);
    let ringSize = 1;
    while (ringSize < (lookN + postN) * 2) ringSize *= 2;
    const ring = new Float32Array(ringSize);
    const envRing = new Float32Array(ringSize);
    const mask = ringSize - 1;
    const warmup = Math.round(WARMUP * sr);

    let pos = 0;
    let env = 0;
    let noise = null;
    let over = 0;
    let threshold = FLOOR;
    let peaks = [];
    let peakRef = 0;
    let lastBeatAt = 0;
    let armedAt = 0;
    let pending = [];
    let level = 0;
    let rejected = 0;
    let loudStreak = 0;

    let period = null;
    let auto = bphSetting === "auto";
    let doubt = false;
    let onsets = [];
    let hits = [];
    let beats = [];
    let prev = null;
    let t0 = null;
    let events = [];

    function lock(bph, isAuto) {
      period = periodOf(bph);
      auto = isAuto;
      doubt = false;
      hits = [];
      // onsets before the lock may have been noise; judge knocks by beats
      peaks = [];
      peakRef = 0;
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
    // the choice is made again on fresh onsets.
    function chooseRate() {
      if (period !== null && !doubt) {
        if (onsets.length >= AUTO_ONSETS && scorePeriod(period, onsets) < LOCK_MIN) {
          doubt = true;
          onsets = [];
        }
        return;
      }
      if (onsets.length < AUTO_MIN) return;
      if (period !== null && scorePeriod(period, onsets) >= LOCK_MIN) {
        doubt = false;
        return;
      }
      // replacing a locked rate takes a full window, not a short burst
      if (period !== null && onsets.length < AUTO_ONSETS) return;
      const best = bestBph(onsets);
      if (best.score >= LOCK_MIN) lock(best.bph, true);
    }

    /**
     * Track how many of the beats due actually got through the gate. A
     * different watch mostly lands outside it, so too few means the gate is
     * holding the wrong rate: drop it and choose the rate again on ungated
     * onsets (a ringing watch still hits nearly every beat).
     */
    function checkHits(k) {
      hits.push(k);
      if (hits.length > HIT_BEATS) hits.shift();
      if (hits.length < HIT_BEATS) return;
      const due = hits.reduce((a, b) => a + b, 0);
      if (hits.length / due < HIT_MIN) {
        doubt = true;
        onsets = [];
        hits = [];
      }
    }

    function onBeat(t, peakAmp, wave) {
      onsets.push(t);
      if (onsets.length > AUTO_ONSETS) onsets.shift();
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
        if (auto && !doubt) checkHits(k);
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
      // beats older than the window are dropped, so a full window spans just
      // under it
      const span = beats[beats.length - 1].t - beats[0].t;
      return {
        type: "reading",
        t,
        rate: 86400 * (period / f.b - 1) + correction,
        beatError: 2 * Math.abs(f.h) * 1000,
        bph: Math.round(3600 / period),
        span,
        window: average,
        settled: span >= average - SETTLE_SLACK * period,
      };
    }

    function step(x) {
      const y = hp(x);
      ring[pos & mask] = y;
      const prevEnv = env;
      env = envA * env + (1 - envA) * Math.abs(y);
      envRing[pos & mask] = env;
      if (noise === null) noise = env;
      if (env > level) level = env;

      for (const p of pending) if (env > p.envPeak) p.envPeak = env;

      // forget the beat peaks of a watch that has gone quiet
      if (peaks.length && pos - lastBeatAt > silenceN) {
        peaks = [];
        peakRef = 0;
      }
      threshold = Math.max(FLOOR, noise + Math.max(mult * noise, PEAK_FRAC * (peakRef - noise)));
      over = env >= threshold ? over + 1 : 0;
      const na = pos < warmup || over > sustainedN ? warmA : over === 0 ? quietA : loudA;
      noise = na * noise + (1 - na) * env;
      if (pos >= warmup && pos >= armedAt && prevEnv < threshold && env >= threshold && inGate()) {
        const frac = (threshold - prevEnv) / (env - prevEnv);
        const onset = pos - 1 + frac;
        const gap = period === null ? MIN_GAP : Math.max(MIN_GAP, GAP_FRAC * period);
        armedAt = pos + Math.round(gap * sr);
        pending.push({ onset, at: pos, envPeak: env });
      }

      if (pending.length && pos >= pending[0].at + postN) {
        const p = pending.shift();
        const loud = period !== null && peaks.length >= LOUD_MIN_BEATS && p.envPeak > LOUD_MULT * peakRef;
        if (loud && loudStreak < LOUD_STREAK) {
          // a knock: skip it and listen again right away
          rejected++;
          loudStreak++;
          armedAt = pos;
        } else {
          if (loud) {
            // loud every time: the watch itself got louder (moved closer)
            peaks = [];
          }
          loudStreak = 0;
          const wave = new Float32Array(preN + postN);
          const start = p.at - preN;
          for (let i = 0; i < wave.length; i++) wave[i] = ring[(start + i) & mask];
          peaks.push(p.envPeak);
          if (peaks.length > PEAKS) peaks.shift();
          const sorted = [...peaks].sort((a, b) => a - b);
          peakRef = sorted[sorted.length >> 1];
          lastBeatAt = pos;
          onBeat(timeBeat(p) / sr, p.envPeak, wave);
        }
      }
      pos++;
    }

    /**
     * A beat's time, by constant fraction: where its envelope first reaches
     * CFD_FRAC of the way from the noise floor to this beat's own peak (taken
     * from the trigger on, so an earlier tail can't inflate it). The search
     * starts where this beat's rise began, stepping back from the trigger up
     * to 12 ms (a late trigger may have skipped the unlock) but not past a
     * dip under the level, so the tail of the beat before is never picked.
     * The trigger crossing alone moves with the beat's loudness against the
     * threshold, and near the noise it jumps between the unlock, impulse,
     * and drop sounds.
     */
    function timeBeat(p) {
      const from = p.at - lookN;
      const to = p.at + postN;
      let top = 0;
      for (let i = p.at; i < to; i++) top = Math.max(top, envRing[i & mask]);
      const level = noise + CFD_FRAC * (top - noise);
      let start = p.at;
      while (start > from && envRing[(start - 1) & mask] >= level) start--;
      for (let i = Math.max(start, from + 1); i < to; i++) {
        const a = envRing[(i - 1) & mask];
        const b = envRing[i & mask];
        if (a < level && b >= level) return i - 1 + (level - a) / (b - a);
      }
      return p.onset;
    }

    /** Input level (envelope peak since the last call), noise floor, and threshold. */
    function levels() {
      const out = { level, noise: noise || 0, threshold, rejected };
      level = 0;
      return out;
    }

    /**
     * Whether a trigger now could be the next beat. While beats are being
     * tracked, only near a whole number of periods after the last one: a
     * case or table ringing on after a beat would otherwise trigger mid
     * period, count as a beat, and lock out the real one after it. With no
     * beat for a few periods the gate opens again.
     */
    function inGate() {
      if (period === null || !prev || doubt) return true;
      const phase = (pos / sr - prev.t) / period;
      if (phase > GATE_PERIODS) return true;
      return phase >= 1 - GATE && Math.abs(phase - Math.round(phase)) <= GATE;
    }

    function push(samples) {
      events = [];
      for (let i = 0; i < samples.length; i++) step(samples[i]);
      return events;
    }

    function setBph(bph) {
      bphSetting = bph;
      onsets = [];
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
      setSensitivity: (name) => {
        mult = SENSITIVITY[name] || SENSITIVITY.normal;
      },
      levels,
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
  // a case or table ringing on after the drop: two close tones, so the tail
  // swells and fades as they beat
  const RING_START = 0.009;
  const RING_HZ = [3000, 3060];

  /**
   * A synthesized watch (TICK-5): `next(count)` returns the next `count`
   * samples. `rate` is in s/d (fast positive), `beatError` in ms (odd beats
   * land late by it), `noise` is the white noise level, and `drop` is the
   * chance a beat is silent. `ring` adds a resonant tail at that fraction of
   * the drop's level, decaying with `ringTau` seconds.
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
    const ring = opts.ring || 0;
    const ringTau = opts.ringTau || 0.02;
    const beatLen = ring ? Math.max(BEAT_LEN, RING_START + 6 * ringTau) : BEAT_LEN;
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
        const first = Math.max(0, Math.floor((t - start - beatLen - beatError) / period));
        const last = Math.floor((t - start) / period);
        for (let k = first; k <= last; k++) {
          const tk = start + k * period + (k % 2 ? beatError : 0);
          const dt = t - tk;
          if (dt < 0 || dt >= beatLen || (drop && isSilent(k))) continue;
          for (const [off, level, hz] of BURSTS) {
            const u = dt - off;
            if (u >= 0 && u < BEAT_LEN) v += amp * level * Math.sin(2 * Math.PI * hz * u) * Math.exp(-u / BURST_TAU);
          }
          const u = dt - RING_START;
          if (ring && u >= 0) {
            const tone = Math.sin(2 * Math.PI * RING_HZ[0] * u) + Math.sin(2 * Math.PI * RING_HZ[1] * u);
            v += amp * ring * 0.5 * tone * Math.exp(-u / ringTau);
          }
        }
        out[i] = v;
      }
      return out;
    }

    return { next, sampleRate: sr };
  }

  const api = { BPH, SENSITIVITY, createDetector, synth, scorePeriod, bestBph, fit, robustFit, highpass, periodOf };
  if (typeof module === "object" && module.exports) {
    module.exports = api;
  } else {
    root.TickDetector = api;
  }
})(typeof window === "undefined" ? globalThis : window);
