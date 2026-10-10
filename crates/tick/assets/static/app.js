// tick page: runs the microphone or the simulator through the detector and
// draws the rate graph, beat trace, and beat scope (spec/tick.md).
//
// `createTick(win)` takes the window (document, navigator, audio classes,
// timers, storage) so tests can drive it under jsdom; a browser starts it at
// the bottom of this file.
(function (root) {
  "use strict";

  const D = typeof module === "object" && module.exports ? require("./detector.js") : root.TickDetector;

  const SETTINGS_KEY = "tick.settings";
  const DEFAULTS = { bph: "auto", average: 10, span: 60, correction: 0, sensitivity: "normal" };
  const SENSITIVITIES = Object.keys(D.SENSITIVITY);
  // the input meter spans -100 to 0 dBFS of the filtered envelope
  const METER_FLOOR_DB = -100;
  const METER_FALL = 0.85;
  // this many frames of exact zeros means the microphone sends nothing
  const DEAD_FRAMES = 60;
  const AVERAGES = [4, 10, 30, 60];
  const SPANS = [30, 60, 300];
  const MAX_CORRECTION = 100;
  const RANGES = [5, 10, 20, 30, 60, 120, 300, 600];
  const RANGE_MARGIN = 1.15;
  const TRACE_MIN_MS = 2;
  const SCOPE_WAVES = 16;
  const BLOCK = 2048;
  // the simulated watch (TICK-5)
  const SIM = { bph: 28800, rate: 6, beatError: 0.4, noise: 0.003 };
  const SIM_RATE = 48000;
  const SIM_TICK_MS = 50;
  // readings further apart than this are drawn as separate runs
  const LINE_GAP = 2;
  // readings from a window still filling (TICK-23)
  const SETTLING_ALPHA = 0.45;
  const SETTLING_DASH = [6, 4];
  const PROGRESS_W = 160;
  // shared horizontal plot margins, so the rate graph and the beat trace line
  // up in time
  const PAD_L = 48;
  const PAD_R = 14;
  const C = {
    grid: "#1a2620",
    gridText: "#5d7668",
    zero: "#3d5a4a",
    line: "#7cfc9a",
    fast: "rgba(124, 252, 154, 0.05)",
    slow: "rgba(255, 107, 107, 0.04)",
    even: "#ffb547",
    odd: "#4fd1c5",
    text: "#8fa99a",
  };

  /** Settings from storage, with anything missing or invalid at its default. */
  function sanitize(s) {
    const out = { ...DEFAULTS };
    if (!s || typeof s !== "object") return out;
    if (s.bph === "auto" || D.BPH.includes(s.bph)) out.bph = s.bph;
    if (AVERAGES.includes(s.average)) out.average = s.average;
    if (SPANS.includes(s.span)) out.span = s.span;
    if (SENSITIVITIES.includes(s.sensitivity)) out.sensitivity = s.sensitivity;
    if (typeof s.correction === "number" && Number.isFinite(s.correction)) {
      out.correction = Math.max(-MAX_CORRECTION, Math.min(MAX_CORRECTION, s.correction));
    }
    return out;
  }

  /** The smallest symmetric range that holds `peak` with margin (TICK-15). */
  function pickRange(peak) {
    for (const r of RANGES) if (peak * RANGE_MARGIN <= r) return r;
    return RANGES[RANGES.length - 1];
  }

  const signed = (v, digits = 1) => (v > 0 ? "+" : v < 0 ? "-" : "") + Math.abs(v).toFixed(digits);

  function fmtRate(v) {
    return Math.abs(v) < 0.05 ? "0.0" : signed(v);
  }

  /** An envelope level in dBFS, floored at the meter's bottom. */
  function toDb(v) {
    return v > 0 ? Math.max(METER_FLOOR_DB, 20 * Math.log10(v)) : METER_FLOOR_DB;
  }

  /** Where a level sits on the meter, in percent. */
  function meterPct(v) {
    return ((toDb(v) - METER_FLOOR_DB) / -METER_FLOOR_DB) * 100;
  }

  function createTick(win) {
    const doc = win.document;
    const $ = (id) => doc.getElementById(id);
    const el = {
      status: $("status"),
      error: $("error"),
      start: $("start"),
      sim: $("sim"),
      stop: $("stop"),
      bph: $("set-bph"),
      average: $("set-average"),
      span: $("set-span"),
      correction: $("set-correction"),
      sensitivity: $("set-sensitivity"),
      meterDb: $("meter-db"),
      meterLevel: $("meter-level"),
      meterNoise: $("meter-noise"),
      meterThreshold: $("meter-threshold"),
      meterNote: $("meter-note"),
      rateOut: $("rate"),
      beatErrorOut: $("beat-error"),
      bphOut: $("bph"),
      beatsOut: $("beats"),
      rateCanvas: $("rate-canvas"),
      traceCanvas: $("trace-canvas"),
      scopeCanvas: $("scope-canvas"),
    };
    // css sizes from the markup, for when layout reports nothing
    const baseSize = new Map(
      [el.rateCanvas, el.traceCanvas, el.scopeCanvas].map((c) => [c, { w: c.width, h: c.height }]),
    );

    let settings = loadSettings();
    let det = null;
    let source = null;
    let startGen = 0;
    let frame = null;
    let now = 0;
    let readings = [];
    let beats = [];
    let waves = [];
    let beatCount = 0;
    let locked = null;
    let shownLevel = 0;
    let deadFrames = 0;
    // beats heard since the source started, indexed or not
    let heard = 0;

    function loadSettings() {
      try {
        return sanitize(JSON.parse(win.localStorage.getItem(SETTINGS_KEY)));
      } catch {
        return { ...DEFAULTS };
      }
    }

    function saveSettings() {
      try {
        win.localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
      } catch {
        // storage blocked: settings last for this page only
      }
    }

    function showSettings() {
      el.bph.value = String(settings.bph);
      el.average.value = String(settings.average);
      el.span.value = String(settings.span);
      el.correction.value = String(settings.correction);
      el.sensitivity.value = settings.sensitivity;
    }

    function clearData() {
      readings = [];
      beats = [];
      waves = [];
      beatCount = 0;
    }

    function showError(msg) {
      el.error.textContent = msg;
      el.error.hidden = !msg;
    }

    // ---------------------------------------------------------------------
    // sources
    // ---------------------------------------------------------------------

    function begin(sampleRate, stopFn) {
      det = D.createDetector({
        sampleRate,
        bph: settings.bph,
        average: settings.average,
        correction: settings.correction,
        sensitivity: settings.sensitivity,
      });
      locked = settings.bph === "auto" ? null : { bph: settings.bph, auto: false };
      now = 0;
      shownLevel = 0;
      deadFrames = 0;
      heard = 0;
      clearData();
      source = { stop: stopFn };
      showError("");
      render();
    }

    function feed(chunk) {
      if (!det) return;
      for (const ev of det.push(chunk)) handle(ev);
      now = det.time();
      const keep = now - SPANS[SPANS.length - 1] - 1;
      while (readings.length && readings[0].t < keep) readings.shift();
      while (beats.length && beats[0].t < keep) beats.shift();
      schedule();
    }

    function handle(ev) {
      if (ev.type === "lock") {
        locked = { bph: ev.bph, auto: ev.auto };
        clearData();
      } else if (ev.type === "beat") {
        heard++;
        waves.push(ev);
        if (waves.length > SCOPE_WAVES) waves.shift();
        if (ev.n !== null) {
          beatCount++;
          beats.push({ t: ev.t, parity: ev.parity, offsetMs: ev.offsetMs });
        }
      } else {
        readings.push(ev);
      }
    }

    function stop() {
      startGen++;
      if (source) {
        source.stop();
        source = null;
      }
      render();
    }

    async function startMic() {
      stop();
      const gen = startGen;
      const md = win.navigator.mediaDevices;
      if (!md || !md.getUserMedia) {
        showError("The microphone needs a secure (https) page in a browser with getUserMedia.");
        return;
      }
      let stream;
      try {
        stream = await md.getUserMedia({
          audio: { echoCancellation: false, noiseSuppression: false, autoGainControl: false, channelCount: 1 },
          video: false,
        });
      } catch (e) {
        showError(
          e && e.name === "NotAllowedError"
            ? "Microphone permission was denied."
            : `Could not open the microphone: ${(e && e.message) || e}`,
        );
        return;
      }
      const release = () => stream.getTracks().forEach((t) => t.stop());
      if (gen !== startGen) return release();
      let ctx = null;
      try {
        const Ctx = win.AudioContext || win.webkitAudioContext;
        ctx = new Ctx();
        const input = ctx.createMediaStreamSource(stream);
        let node;
        if (ctx.audioWorklet && win.AudioWorkletNode) {
          await ctx.audioWorklet.addModule("/tick/static/worklet.js");
          node = new win.AudioWorkletNode(ctx, "tick-capture");
          node.port.onmessage = (e) => feed(e.data);
        } else {
          node = ctx.createScriptProcessor(BLOCK, 1, 1);
          node.onaudioprocess = (e) => feed(new Float32Array(e.inputBuffer.getChannelData(0)));
        }
        input.connect(node);
        // keep the node pulled by the graph without playing anything back
        const mute = ctx.createGain();
        mute.gain.value = 0;
        node.connect(mute);
        mute.connect(ctx.destination);
        if (ctx.resume) await ctx.resume();
      } catch (e) {
        release();
        if (ctx) ctx.close();
        showError(`Audio setup failed: ${(e && e.message) || e}`);
        return;
      }
      if (gen !== startGen) {
        release();
        ctx.close();
        return;
      }
      begin(ctx.sampleRate, () => {
        release();
        ctx.close();
      });
    }

    function startSim() {
      stop();
      const gen = D.synth({ sampleRate: SIM_RATE, ...SIM, seed: (Math.random() * 2 ** 31) | 0 });
      let last = win.performance.now();
      let carry = 0;
      const timer = win.setInterval(() => {
        const t = win.performance.now();
        // a backgrounded tab catches up by at most a second
        const due = Math.min(((t - last) / 1000) * SIM_RATE + carry, SIM_RATE);
        last = t;
        const n = Math.floor(due);
        carry = due - n;
        if (n > 0) feed(gen.next(n));
      }, SIM_TICK_MS);
      begin(SIM_RATE, () => win.clearInterval(timer));
    }

    // ---------------------------------------------------------------------
    // drawing
    // ---------------------------------------------------------------------

    function schedule() {
      if (frame !== null) return;
      const raf = win.requestAnimationFrame
        ? (fn) => win.requestAnimationFrame(fn)
        : (fn) => win.setTimeout(fn, 16);
      frame = raf(() => {
        frame = null;
        render();
      });
    }

    function render() {
      renderReadout();
      drawRate();
      drawTrace();
      drawScope();
    }

    /**
     * How far the averaging window has filled, as `{ span, window }` in
     * seconds: from the latest reading, or before the first one from the
     * beats indexed since the lock.
     */
    function progress() {
      const r = readings[readings.length - 1];
      if (r) return { span: r.span, window: r.window };
      const span = beats.length > 1 ? beats[beats.length - 1].t - beats[0].t : 0;
      return { span, window: settings.average };
    }

    /** Whether the latest reading fills its averaging window. */
    const settled = () => {
      const r = readings[readings.length - 1];
      return !!r && r.settled;
    };

    const progressText = () => {
      const { span, window } = progress();
      return `measuring: ${Math.min(span, window).toFixed(1)} of ${window} s`;
    };

    /** Status: idle, listening, measuring (locked, window filling), or locked. */
    function status() {
      if (!source) return "idle";
      if (!locked) return "listening";
      return settled() ? "locked" : "measuring";
    }

    function renderReadout() {
      const r = readings[readings.length - 1];
      el.rateOut.textContent = r ? fmtRate(r.rate) : "--";
      el.rateOut.dataset.state = r && !r.settled ? "settling" : "";
      el.beatErrorOut.textContent = r ? r.beatError.toFixed(1) : "--";
      el.bphOut.textContent = locked ? `${locked.bph}${locked.auto ? " auto" : ""}` : "--";
      el.beatsOut.textContent = String(beatCount);
      const state = status();
      el.status.textContent = state;
      el.status.dataset.state = state;
      el.start.disabled = !!source;
      el.sim.disabled = !!source;
      el.stop.disabled = !source;
      renderMeter();
    }

    function renderMeter() {
      if (!source) {
        el.meterDb.textContent = "--";
        el.meterLevel.style.width = "0%";
        el.meterNote.textContent = "";
        return;
      }
      const lv = det.levels();
      shownLevel = Math.max(lv.level, shownLevel * METER_FALL);
      deadFrames = lv.level === 0 ? deadFrames + 1 : 0;
      el.meterDb.textContent = `${Math.round(toDb(shownLevel))} dB`;
      el.meterLevel.style.width = `${meterPct(shownLevel).toFixed(1)}%`;
      el.meterNoise.style.left = `${meterPct(lv.noise).toFixed(1)}%`;
      el.meterThreshold.style.left = `${meterPct(lv.threshold).toFixed(1)}%`;
      el.meterNote.textContent =
        deadFrames > DEAD_FRAMES
          ? "No audio from the microphone. Check the input device and its volume."
          : lv.rejected
            ? `${lv.rejected} loud ${lv.rejected === 1 ? "sound" : "sounds"} ignored`
            : "";
    }

    /** Size a canvas to its css box at device pixel ratio; null without 2d. */
    function surface(canvas) {
      const ctx = canvas.getContext("2d");
      if (!ctx) return null;
      const base = baseSize.get(canvas);
      const w = canvas.clientWidth || base.w;
      const h = canvas.clientHeight || base.h;
      const dpr = win.devicePixelRatio || 1;
      const pw = Math.round(w * dpr);
      const ph = Math.round(h * dpr);
      if (canvas.width !== pw) canvas.width = pw;
      if (canvas.height !== ph) canvas.height = ph;
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, w, h);
      ctx.font = "11px ui-monospace, Menlo, Consolas, monospace";
      return { ctx, w, h };
    }

    /** x for a time on the shared axis of the rate graph and the trace. */
    function timeAxis(w) {
      const span = settings.span;
      const t1 = Math.max(now, span);
      const t0 = t1 - span;
      const plot = w - PAD_L - PAD_R;
      return { t0, t1, x: (t) => PAD_L + ((t - t0) / span) * plot };
    }

    function drawTimeGrid(ctx, w, h, axis, labels) {
      const step = settings.span >= 300 ? 60 : 10;
      ctx.strokeStyle = C.grid;
      ctx.fillStyle = C.gridText;
      ctx.lineWidth = 1;
      ctx.textAlign = "center";
      for (let ago = 0; ago <= settings.span; ago += step) {
        const x = Math.round(axis.x(axis.t1 - ago)) + 0.5;
        ctx.beginPath();
        ctx.moveTo(x, 0);
        ctx.lineTo(x, h - (labels ? 14 : 0));
        ctx.stroke();
        if (labels) ctx.fillText(ago === 0 ? "now" : step === 60 ? `-${ago / 60}m` : `-${ago}s`, x, h - 2);
      }
    }

    function drawRate() {
      const s = surface(el.rateCanvas);
      if (!s) return;
      const { ctx, w, h } = s;
      const axis = timeAxis(w);
      const plotH = h - 16;
      const visible = readings.filter((r) => r.t >= axis.t0);
      const range = pickRange(visible.reduce((m, r) => Math.max(m, Math.abs(r.rate)), 0));
      const mid = plotH / 2;
      const half = mid - 8;
      const y = (v) => mid - (v / range) * half;

      ctx.fillStyle = C.fast;
      ctx.fillRect(PAD_L, mid - half, w - PAD_L - PAD_R, half);
      ctx.fillStyle = C.slow;
      ctx.fillRect(PAD_L, mid, w - PAD_L - PAD_R, half);
      drawTimeGrid(ctx, w, h, axis, true);

      ctx.textAlign = "right";
      ctx.textBaseline = "middle";
      for (const v of [range, range / 2, -range / 2, -range]) {
        const yy = Math.round(y(v)) + 0.5;
        ctx.strokeStyle = C.grid;
        ctx.beginPath();
        ctx.moveTo(PAD_L, yy);
        ctx.lineTo(w - PAD_R, yy);
        ctx.stroke();
        ctx.fillStyle = C.gridText;
        ctx.fillText(signed(v, v % 1 ? 1 : 0), PAD_L - 6, yy);
      }
      ctx.strokeStyle = C.zero;
      ctx.lineWidth = 2;
      ctx.beginPath();
      ctx.moveTo(PAD_L, Math.round(mid));
      ctx.lineTo(w - PAD_R, Math.round(mid));
      ctx.stroke();
      ctx.fillStyle = C.text;
      ctx.fillText("0", PAD_L - 6, mid);
      ctx.textAlign = "left";
      ctx.fillStyle = C.gridText;
      ctx.fillText("fast", PAD_L + 6, mid - half + 8);
      ctx.fillText("slow", PAD_L + 6, mid + half - 8);

      const cx = PAD_L + (w - PAD_L - PAD_R) / 2;
      if (!visible.length) {
        ctx.textAlign = "center";
        ctx.fillStyle = C.text;
        let msg = "start the mic or simulate";
        if (source && !heard) msg = "listening for beats...";
        else if (source && !locked) msg = "ticks heard, finding the beat rate...";
        else if (source) msg = progressText();
        ctx.fillText(msg, cx, mid - 16);
        if (source && locked) drawProgress(ctx, cx, mid - 4);
        return;
      }

      // runs of readings: split at gaps, and where settling turns to settled
      // (settling runs draw dashed and dim, TICK-23)
      const runs = [];
      visible.forEach((r, i) => {
        const prev = visible[i - 1];
        if (!prev || r.t - prev.t > LINE_GAP || prev.settled !== r.settled) {
          runs.push({ settled: r.settled, points: prev && r.t - prev.t <= LINE_GAP ? [prev] : [] });
        }
        runs[runs.length - 1].points.push(r);
      });
      ctx.strokeStyle = C.line;
      ctx.lineWidth = 2;
      ctx.lineJoin = "round";
      ctx.shadowColor = C.line;
      for (const run of runs) {
        ctx.globalAlpha = run.settled ? 1 : SETTLING_ALPHA;
        ctx.setLineDash(run.settled ? [] : SETTLING_DASH);
        ctx.shadowBlur = run.settled ? 6 : 0;
        ctx.beginPath();
        run.points.forEach((r, i) => {
          if (i === 0) ctx.moveTo(axis.x(r.t), y(r.rate));
          else ctx.lineTo(axis.x(r.t), y(r.rate));
        });
        ctx.stroke();
      }
      ctx.setLineDash([]);

      const head = visible[visible.length - 1];
      const hx = axis.x(head.t);
      const hy = y(head.rate);
      ctx.globalAlpha = head.settled ? 1 : SETTLING_ALPHA;
      ctx.shadowBlur = head.settled ? 6 : 0;
      ctx.fillStyle = C.line;
      ctx.beginPath();
      ctx.arc(hx, hy, 4, 0, Math.PI * 2);
      ctx.fill();
      ctx.shadowBlur = 0;
      ctx.globalAlpha = 1;
      if (!head.settled && source) {
        ctx.textAlign = "center";
        ctx.textBaseline = "middle";
        ctx.fillStyle = C.text;
        ctx.fillText(progressText(), cx, 14);
        drawProgress(ctx, cx, 26);
        ctx.fillStyle = C.line;
      }
      ctx.textAlign = "right";
      ctx.textBaseline = hy < mid ? "top" : "bottom";
      ctx.fillText(fmtRate(head.rate), hx - 8, hy + (hy < mid ? 6 : -6));
    }

    /** A bar showing how far the averaging window has filled, centered on x. */
    function drawProgress(ctx, x, y) {
      const { span, window } = progress();
      const frac = Math.min(1, span / window);
      ctx.fillStyle = C.grid;
      ctx.fillRect(x - PROGRESS_W / 2, y, PROGRESS_W, 4);
      ctx.fillStyle = C.line;
      ctx.fillRect(x - PROGRESS_W / 2, y, PROGRESS_W * frac, 4);
    }

    function drawTrace() {
      const s = surface(el.traceCanvas);
      if (!s) return;
      const { ctx, w, h } = s;
      const axis = timeAxis(w);
      drawTimeGrid(ctx, w, h, axis, false);
      const visible = beats.filter((b) => b.t >= axis.t0);
      if (!visible.length) return;
      let lo = Infinity;
      let hi = -Infinity;
      for (const b of visible) {
        lo = Math.min(lo, b.offsetMs);
        hi = Math.max(hi, b.offsetMs);
      }
      const spread = Math.max(TRACE_MIN_MS, (hi - lo) * 1.2);
      const center = (hi + lo) / 2;
      const y = (v) => h / 2 - ((v - center) / spread) * (h - 16);

      ctx.fillStyle = C.gridText;
      ctx.textAlign = "right";
      ctx.textBaseline = "middle";
      ctx.fillText(`+${(spread / 2).toFixed(1)}`, PAD_L - 6, 10);
      ctx.fillText(`-${(spread / 2).toFixed(1)}`, PAD_L - 6, h - 10);
      ctx.fillText("ms", PAD_L - 6, h / 2);

      for (const b of visible) {
        ctx.fillStyle = b.parity ? C.odd : C.even;
        ctx.fillRect(axis.x(b.t) - 1, y(b.offsetMs) - 1, 2.5, 2.5);
      }
    }

    function drawScope() {
      const s = surface(el.scopeCanvas);
      if (!s) return;
      const { ctx, w, h } = s;
      const ms = det ? det.waveMs : { pre: 2, post: 30 };
      const total = ms.pre + ms.post;
      const x = (m) => 6 + ((m + ms.pre) / total) * (w - 12);

      ctx.strokeStyle = C.grid;
      ctx.fillStyle = C.gridText;
      ctx.lineWidth = 1;
      ctx.textAlign = "center";
      ctx.textBaseline = "alphabetic";
      for (let m = 0; m <= ms.post; m += 10) {
        const xx = Math.round(x(m)) + 0.5;
        ctx.beginPath();
        ctx.moveTo(xx, 0);
        ctx.lineTo(xx, h - 14);
        ctx.stroke();
        ctx.fillText(String(m), xx, h - 2);
      }
      const mid = (h - 14) / 2;
      ctx.beginPath();
      ctx.moveTo(0, mid);
      ctx.lineTo(w, mid);
      ctx.stroke();

      if (!waves.length) return;
      let peak = 0;
      for (const wv of waves) for (const v of wv.wave) peak = Math.max(peak, Math.abs(v));
      if (!peak) return;
      waves.forEach((wv, i) => {
        ctx.globalAlpha = 0.15 + (0.85 * (i + 1)) / waves.length;
        ctx.strokeStyle = wv.parity === 1 ? C.odd : C.even;
        ctx.lineWidth = i === waves.length - 1 ? 1.5 : 1;
        ctx.beginPath();
        const n = wv.wave.length;
        // one point per pixel column is plenty
        const stride = Math.max(1, Math.floor(n / (w * 2)));
        for (let j = 0; j < n; j += stride) {
          const px = x((j / n) * total - ms.pre);
          const py = mid - (wv.wave[j] / peak) * (mid - 4);
          if (j === 0) ctx.moveTo(px, py);
          else ctx.lineTo(px, py);
        }
        ctx.stroke();
      });
      ctx.globalAlpha = 1;
    }

    // ---------------------------------------------------------------------
    // wiring
    // ---------------------------------------------------------------------

    function onSetting(key, value) {
      settings[key] = value;
      settings = sanitize(settings);
      saveSettings();
      if (det) {
        if (key === "bph") {
          for (const ev of det.setBph(settings.bph)) handle(ev);
          if (settings.bph === "auto") locked = null;
          clearData();
        } else if (key === "average") {
          det.setAverage(settings.average);
        } else if (key === "correction") {
          det.setCorrection(settings.correction);
        } else if (key === "sensitivity") {
          det.setSensitivity(settings.sensitivity);
        }
      }
      render();
    }

    function start() {
      showSettings();
      el.start.addEventListener("click", startMic);
      el.sim.addEventListener("click", startSim);
      el.stop.addEventListener("click", stop);
      el.bph.addEventListener("change", () =>
        onSetting("bph", el.bph.value === "auto" ? "auto" : Number(el.bph.value)),
      );
      el.average.addEventListener("change", () => onSetting("average", Number(el.average.value)));
      el.span.addEventListener("change", () => onSetting("span", Number(el.span.value)));
      el.sensitivity.addEventListener("change", () => onSetting("sensitivity", el.sensitivity.value));
      el.correction.addEventListener("change", () => {
        onSetting("correction", Number(el.correction.value) || 0);
        el.correction.value = String(settings.correction);
      });
      win.addEventListener("resize", schedule);
      render();
    }

    return {
      start,
      stop,
      startMic,
      startSim,
      feed,
      // for tests
      state: () => ({ settings, readings, beats, waves, beatCount, locked, running: !!source }),
    };
  }

  if (typeof module === "object" && module.exports) {
    module.exports = { createTick, sanitize, pickRange, fmtRate, toDb, meterPct, DEFAULTS };
  } else {
    createTick(root).start();
  }
})(typeof window === "undefined" ? globalThis : window);
