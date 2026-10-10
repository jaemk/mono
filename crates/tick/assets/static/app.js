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
  const SESSION_KEY = "tick.session";
  const DEFAULTS = { bph: "auto", average: 10, span: 60, correction: 0, sensitivity: "normal", target: "off" };
  // target bands for the rate, [slow, fast] in s/d (TICK-30)
  const TARGETS = { off: null, cosc: [-4, 6], 5: [-5, 5], 10: [-10, 10], 20: [-20, 20], 30: [-30, 30] };
  // beat error past these (ms) reads as worth a look, then as out of beat
  const BEAT_ERROR_WARN = 1;
  const BEAT_ERROR_BAD = 3;
  // the positions a watch is timed in (TICK-29), in table order
  const POSITIONS = {
    DU: "dial up",
    DD: "dial down",
    CU: "crown up",
    CD: "crown down",
    CL: "crown left",
    CR: "crown right",
  };
  // full screen mode's trend compares the rate with this many seconds ago,
  // and calls a change under TREND_STEADY s/d steady (TICK-28)
  const TREND_SECS = 5;
  const TREND_STEADY = 1;
  // below this width the settings fold away (TICK-33)
  const NARROW = "(max-width: 640px)";
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
  // the record button saves this much raw input as a wav (TICK-24)
  const RECORD_SECS = 30;
  // files are decoded at this rate and analyzed up to this long (TICK-25)
  const FILE_RATE = 48000;
  const MAX_ANALYZE_SECS = 300;
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
    band: "rgba(124, 252, 154, 0.09)",
    bandEdge: "rgba(124, 252, 154, 0.35)",
    cursor: "#cfe3d6",
  };

  /**
   * What each graph and value shows (TICK-26), opened from its i button
   * (`data-info` names the entry). Each body entry is one paragraph.
   */
  const INFO = {
    status: {
      title: "Status",
      body: [
        "idle: nothing is running. listening: sound is coming in, but no beat rate yet (it needs about 8 beats in a steady rhythm). measuring: the beat rate is known and the averaging window is still filling, so the rate is provisional. locked: the window is full and the rate is settled. analyzed: a finished recording or file is on show instead of live input.",
      ],
    },
    rate: {
      title: "Rate",
      body: [
        "How many seconds a day the watch gains (+, fast) or loses (-, slow) at the rate it is running now. Live, it is fit over the beats in the averaging window, and it is dimmed until that window has filled. For a recording or file, it is one fit over the whole of it.",
        "It comes from beat timing alone: each beat's time is compared with where a perfect watch at the nominal beat rate would put it, and the slope of that drift is the rate. +10 s/d means each beat comes about 0.012% early.",
        "As a rough guide, chronometer certification allows -4 to +6 s/d, many modern movements run within about 10 s/d, and older or unserviced watches can be off by 30 s/d or more. Rate changes with position (dial up, crown down, and so on) and with how wound the mainspring is, so time it in a few positions for the full picture.",
        "A computer's sound card clock can itself be off by a few s/d. Time a watch you trust and enter the difference as mic correction to cancel it out.",
      ],
    },
    "beat-error": {
      title: "Beat error",
      body: [
        "How unevenly the tick and the tock are spaced, in milliseconds. A balance that is in beat swings equally far either side of its rest point, so each beat lands exactly halfway between its neighbours and beat error is 0.",
        "As a rough guide, under 0.5 ms is very good and under 1 ms is fine. Above about 3 ms a watchmaker would usually put the watch back in beat; a watch far out of beat can stop more easily as it runs down.",
        "On the beat trace, beat error is the gap between the tick (amber) and tock (teal) lines. It does not affect the rate.",
        "The value turns amber over 1 ms and red over 3 ms.",
      ],
    },
    bph: {
      title: "Beat rate",
      body: [
        "Beats per hour: how many ticks and tocks the movement makes in an hour, fixed by its design. Common rates are 18000 (5 a second, many vintage watches), 21600 (6), 28800 (8, most modern watches), and 36000 (10, high beat).",
        "auto means it was found from the gaps between beats. If it picks the wrong one (say double the real rate, when every tick and tock is heard twice), click the beat rate and choose the right one. A rate you choose stays; so does auto's choice once you hold it. It turns red when the beats stop fitting auto's choice and auto is finding the rate again.",
      ],
    },
    beats: {
      title: "Beats",
      body: [
        "Live: the beats counted since the beat rate locked. For a recording or file: the beats in the longest unbroken run, which is what its rate is fit over.",
        "A missed beat does no harm: each beat is placed by its position in the sequence, so gaps are bridged. Only a silence of more than 3 seconds starts the count over.",
      ],
    },
    input: {
      title: "Input",
      body: [
        "How loud the microphone signal is, in dB below full scale, after a filter that keeps only sound above 2 kHz, where watch ticks are and most room noise is not.",
        "The grey mark is the noise floor (the room's background level) and the amber mark is the trigger threshold. Ticks should peak past the amber mark while the background stays below it. If they don't, move the watch closer to the microphone or raise the sensitivity.",
        "Sounds far louder than the ticks (a knock, a bump, a voice) are ignored rather than counted as beats; the line under the meter counts them. If it counts up with every tick instead, quieter sounds are being taken for beats and the real ticks for noise: lower the sensitivity or quiet the room.",
      ],
    },
    "rate-graph": {
      title: "Rate graph",
      body: [
        "The rate over time in s/d. Zero is the line across the middle; fast is above (green tint) and slow below (red tint). Live, time runs left to right with the newest reading at the right edge; for a recording or file, the graph spans the whole recording from its start.",
        "Each point is the rate over the averaging window ending at that beat, so the line moves smoothly. While the window is still filling, the line is dashed and dim. The vertical scale grows to fit the readings shown.",
        "A real watch's rate wanders a few s/d from second to second as the gear train turns and the balance swing varies. A longer averaging window smooths that out.",
        "With a target set in the settings, the target band is shaded across the graph and the rate turns amber outside it. Point at or tap the graph to read the rate at that moment.",
      ],
    },
    session: {
      title: "Positions",
      body: [
        "A watch runs at a slightly different rate in each position, because gravity pulls on the balance differently. Watchmakers time it in several (commonly dial up, dial down, and two or more crown positions) and regulate for the average.",
        "Put the watch in a position, let the reading settle (status locked) or record and analyze it, pick the position, and press save result. Saving a position again replaces it. The results stay in this browser until cleared.",
        "average is the mean rate and beat error over the saved positions. delta is the fastest position less the slowest: under about 10 s/d is good for a mechanical watch, and a large delta points to a balance out of poise or a worn pivot rather than the regulator setting. copy puts the table on the clipboard as text.",
      ],
    },
    trace: {
      title: "Beat trace",
      body: [
        "One dot per beat, on the same time axis as the rate graph. The height is how early (up) or late (down) the beat came compared with a perfect watch at the nominal beat rate, in milliseconds. Amber dots are ticks and teal dots are tocks.",
        "A watch on time draws flat lines. A fast watch's beats come earlier and earlier, so the lines climb; a slow watch's lines fall. The steeper the lines, the bigger the rate.",
        "The gap between the amber and teal lines is the beat error. Dots scattered away from the lines are beats caught on noise or a weak pickup. Point at or tap a dot to read it.",
      ],
    },
    scope: {
      title: "Beat scope",
      body: [
        "The sound of the last 16 beats overlaid, aligned where each was detected, from 2 ms before to 30 ms after (filtered above 2 kHz). The newest is brightest; amber is a tick and teal is a tock.",
        "A clean beat shows a few distinct bursts within some milliseconds of each other: the escapement unlocking, the impulse to the balance, and the drop as the escape wheel locks again. Shapes that overlay neatly mean a steady pickup. Messy or changing shapes mean noise reaching the microphone, or a watch that may need attention. Ticks and tocks looking somewhat different from each other is normal.",
      ],
    },
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
    if (typeof s.target === "string" && Object.hasOwn(TARGETS, s.target)) out.target = s.target;
    return out;
  }

  /** Saved position results from storage, keeping only well formed rows. */
  function sanitizeSession(rows) {
    if (!Array.isArray(rows)) return [];
    const num = (v) => typeof v === "number" && Number.isFinite(v);
    const out = rows.filter(
      (r) => r && Object.hasOwn(POSITIONS, r.pos) && num(r.rate) && num(r.beatError) && num(r.bph),
    );
    return sortSession(out.filter((r, i) => out.findIndex((o) => o.pos === r.pos) === i));
  }

  const POSITION_ORDER = Object.keys(POSITIONS);
  const sortSession = (rows) => rows.sort((a, b) => POSITION_ORDER.indexOf(a.pos) - POSITION_ORDER.indexOf(b.pos));

  /** "in" or "out" of the target band, or "" with no target. */
  function tolerance(rate, target) {
    const band = TARGETS[target];
    if (!band) return "";
    return rate >= band[0] && rate <= band[1] ? "in" : "out";
  }

  /** "", "warn", or "bad" for a beat error in ms. */
  function beatErrorLevel(ms) {
    return ms > BEAT_ERROR_BAD ? "bad" : ms > BEAT_ERROR_WARN ? "warn" : "";
  }

  /**
   * Which way the rate is heading: the latest reading against the last one
   * at least `TREND_SECS` before it. "" until there is one that old.
   */
  function trend(readings) {
    const last = readings[readings.length - 1];
    if (!last) return "";
    for (let i = readings.length - 2; i >= 0; i--) {
      if (last.t - readings[i].t >= TREND_SECS) {
        const d = last.rate - readings[i].rate;
        return d > TREND_STEADY ? "↑" : d < -TREND_STEADY ? "↓" : "→";
      }
    }
    return "";
  }

  /** Mean rate and beat error, and the fastest less the slowest rate. */
  function sessionStats(rows) {
    if (!rows.length) return null;
    const rates = rows.map((r) => r.rate);
    return {
      rate: rates.reduce((a, b) => a + b, 0) / rows.length,
      beatError: rows.reduce((a, r) => a + r.beatError, 0) / rows.length,
      delta: Math.max(...rates) - Math.min(...rates),
    };
  }

  /** The position table as plain text, for the clipboard. */
  function sessionText(rows) {
    const lines = ["tick positions"];
    for (const r of rows) {
      lines.push(
        `${r.pos}  ${POSITIONS[r.pos].padEnd(11)}  ${fmtRate(r.rate).padStart(7)} s/d  ${r.beatError.toFixed(1).padStart(4)} ms  ${r.bph} bph`,
      );
    }
    const s = sessionStats(rows);
    if (s) {
      lines.push(`average ${fmtRate(s.rate)} s/d, ${s.beatError.toFixed(1)} ms`);
      lines.push(`delta ${s.delta.toFixed(1)} s/d`);
    }
    return lines.join("\n");
  }

  /** The smallest symmetric range that holds `peak` with margin (TICK-15). */
  function pickRange(peak) {
    for (const r of RANGES) if (peak * RANGE_MARGIN <= r) return r;
    return RANGES[RANGES.length - 1];
  }

  const signed = (v, digits = 1) => (v > 0 ? "+" : v < 0 ? "-" : "") + Math.abs(v).toFixed(digits);

  /**
   * Mono 32-bit float WAV (format 3, with the fact chunk non-PCM formats
   * carry), so the samples round trip exactly, however quiet the mic is.
   */
  function encodeWav(samples, sampleRate) {
    const dataBytes = samples.length * 4;
    const buf = new ArrayBuffer(58 + dataBytes);
    const v = new DataView(buf);
    const tag = (at, s) => [...s].forEach((c, i) => v.setUint8(at + i, c.charCodeAt(0)));
    tag(0, "RIFF");
    v.setUint32(4, 50 + dataBytes, true);
    tag(8, "WAVE");
    tag(12, "fmt ");
    v.setUint32(16, 18, true);
    v.setUint16(20, 3, true); // IEEE float
    v.setUint16(22, 1, true); // mono
    v.setUint32(24, sampleRate, true);
    v.setUint32(28, sampleRate * 4, true); // byte rate
    v.setUint16(32, 4, true); // block align
    v.setUint16(34, 32, true); // bits per sample
    v.setUint16(36, 0, true); // no extension
    tag(38, "fact");
    v.setUint32(42, 4, true);
    v.setUint32(46, samples.length, true);
    tag(50, "data");
    v.setUint32(54, dataBytes, true);
    for (let i = 0; i < samples.length; i++) v.setFloat32(58 + i * 4, samples[i], true);
    return new Uint8Array(buf);
  }

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
      run: $("run"),
      sim: $("sim"),
      record: $("record"),
      save: $("save"),
      open: $("open"),
      file: $("file"),
      rateLabel: $("rate-label"),
      info: $("info"),
      infoTitle: $("info-title"),
      infoBody: $("info-body"),
      infoClose: $("info-close"),
      bph: $("set-bph"),
      average: $("set-average"),
      span: $("set-span"),
      correction: $("set-correction"),
      sensitivity: $("set-sensitivity"),
      target: $("set-target"),
      settingsBox: $("settings-box"),
      focus: $("focus"),
      focusView: $("focus-view"),
      focusStatus: $("focus-status"),
      focusRate: $("focus-rate"),
      focusTrend: $("focus-trend"),
      focusBeatError: $("focus-beat-error"),
      focusBph: $("focus-bph"),
      focusDetail: $("focus-detail"),
      focusNote: $("focus-note"),
      bphDialog: $("bph-dialog"),
      bphNow: $("bph-now"),
      bphChoices: $("bph-choices"),
      bphClose: $("bph-close"),
      focusExit: $("focus-exit"),
      focusRun: $("focus-run"),
      focusSettings: $("focus-settings"),
      drop: $("drop"),
      sessionPos: $("session-pos"),
      sessionAdd: $("session-add"),
      sessionCopy: $("session-copy"),
      sessionClear: $("session-clear"),
      sessionNote: $("session-note"),
      sessionBody: $("session-body"),
      sessionAvg: $("session-avg"),
      sessionAvgBe: $("session-avg-be"),
      sessionDelta: $("session-delta"),
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
    // a recording in progress: { buf, len, sampleRate }
    let rec = null;
    // the object url behind the save link of the last recording
    let saveUrl = null;
    // a recording or file on show instead of a live source (TICK-25):
    // { samples, sampleRate, name, truncated, duration, summary }
    let analysis = null;
    let shownLevel = 0;
    let deadFrames = 0;
    // beats heard since the source started, indexed or not
    let heard = 0;
    // the screen wake lock held while a source runs (TICK-27)
    let wake = null;
    let wakePending = false;
    // whether full screen mode asked for browser fullscreen (TICK-28)
    let focusFull = false;
    // where the settings live outside full screen mode, and whether they were open there
    const settingsHome = el.settingsBox.parentNode;
    let settingsWasOpen = true;
    // the startGen of a mic still opening, or -1
    let opening = -1;
    // saved position results (TICK-29)
    let session = loadSession();
    // where the pointer is over the rate graph or beat trace, as a fraction
    // of the plot width (TICK-31)
    let cursor = null;
    // nested dragenter/dragleave pairs over the page (TICK-32)
    let dragDepth = 0;

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

    function loadSession() {
      try {
        return sanitizeSession(JSON.parse(win.localStorage.getItem(SESSION_KEY)));
      } catch {
        return [];
      }
    }

    function saveSession() {
      try {
        win.localStorage.setItem(SESSION_KEY, JSON.stringify(session));
      } catch {
        // storage blocked: results last for this page only
      }
    }

    function showSettings() {
      el.bph.value = String(settings.bph);
      el.average.value = String(settings.average);
      el.span.value = String(settings.span);
      el.correction.value = String(settings.correction);
      el.sensitivity.value = settings.sensitivity;
      el.target.value = settings.target;
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
      source = { stop: stopFn, sampleRate };
      rec = null;
      analysis = null;
      cursor = null;
      showError("");
      holdWake();
      render();
    }

    // ---------------------------------------------------------------------
    // screen wake lock (TICK-27)
    // ---------------------------------------------------------------------

    /** Keep the screen on while a source runs, where the browser allows it. */
    async function holdWake() {
      const wl = win.navigator.wakeLock;
      if (!wl || wake || wakePending || !source || doc.visibilityState === "hidden") return;
      wakePending = true;
      try {
        const sentinel = await wl.request("screen");
        if (source && !wake) {
          wake = sentinel;
          // the browser lets go when the tab is hidden
          sentinel.addEventListener("release", () => {
            if (wake === sentinel) wake = null;
          });
        } else {
          sentinel.release();
        }
      } catch {
        // denied (low battery, no user gesture): the screen may dim
      }
      wakePending = false;
    }

    function dropWake() {
      if (!wake) return;
      const sentinel = wake;
      wake = null;
      sentinel.release();
    }

    function feed(chunk) {
      // blocks still queued from a stopped source change nothing
      if (!source) return;
      if (rec) {
        record(chunk);
        // a finished recording stops the source for its analysis
        if (!source) return;
      }
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
        if (!ev.kept) clearData();
      } else if (ev.type === "beat") {
        heard++;
        waves.push(ev);
        if (waves.length > SCOPE_WAVES) waves.shift();
        if (ev.n !== null) {
          // a better onset for the same beat takes the last one's place
          if (ev.replaced) beats.pop();
          else beatCount++;
          beats.push({ t: ev.t, parity: ev.parity, offsetMs: ev.offsetMs });
        }
      } else {
        readings.push(ev);
      }
    }

    function stop() {
      startGen++;
      rec = null;
      if (source) {
        source.stop();
        source = null;
      }
      dropWake();
      render();
    }

    // ---------------------------------------------------------------------
    // recording (TICK-24)
    // ---------------------------------------------------------------------

    /** Record the next 30 s of the running source, opening the mic if idle. */
    async function startRecording() {
      if (rec) return;
      if (!source) {
        await startMic();
        if (!source) return;
      }
      const sampleRate = source.sampleRate;
      clearSave();
      rec = { buf: new Float32Array(RECORD_SECS * sampleRate), len: 0, sampleRate };
      render();
    }

    /**
     * Keep the raw samples, exactly as the detector gets them. A full
     * recording stops the source, is offered for saving, and is analyzed.
     */
    function record(chunk) {
      const n = Math.min(chunk.length, rec.buf.length - rec.len);
      rec.buf.set(chunk.subarray(0, n), rec.len);
      rec.len += n;
      if (rec.len === rec.buf.length) {
        const done = rec;
        rec = null;
        stop();
        offerSave(encodeWav(done.buf, done.sampleRate), recordingName(), RECORD_SECS);
        showAnalysis(done.buf, done.sampleRate, "recording", false);
      }
    }

    // ---------------------------------------------------------------------
    // analysis of a recording or file (TICK-25)
    // ---------------------------------------------------------------------

    function showAnalysis(samples, sampleRate, name, truncated) {
      analysis = { samples, sampleRate, name, truncated };
      reanalyze();
    }

    /** Run the whole recording through a detector with the current settings. */
    function reanalyze() {
      const a = D.analyze(analysis.samples, {
        sampleRate: analysis.sampleRate,
        bph: settings.bph,
        average: settings.average,
        correction: settings.correction,
        sensitivity: settings.sensitivity,
      });
      locked = settings.bph === "auto" ? null : { bph: settings.bph, auto: false };
      clearData();
      heard = 0;
      for (const ev of a.events) handle(ev);
      analysis.duration = a.duration;
      analysis.summary = a.summary;
      now = a.duration;
      render();
    }

    /** Decode an audio file (mixed to mono) and analyze its first 5 minutes. */
    async function analyzeFile(file) {
      stop();
      const gen = startGen;
      showError("");
      let audio;
      try {
        const Ctx = win.OfflineAudioContext || win.webkitOfflineAudioContext;
        const bytes = await file.arrayBuffer();
        audio = await new Ctx(1, 1, FILE_RATE).decodeAudioData(bytes);
      } catch (e) {
        showError(`Could not read ${file.name} as audio: ${(e && e.message) || e}`);
        return;
      }
      if (gen !== startGen) return;
      const n = Math.min(audio.length, MAX_ANALYZE_SECS * audio.sampleRate);
      const samples = new Float32Array(n);
      for (let c = 0; c < audio.numberOfChannels; c++) {
        const data = audio.getChannelData(c);
        for (let i = 0; i < n; i++) samples[i] += data[i] / audio.numberOfChannels;
      }
      clearSave();
      showAnalysis(samples, audio.sampleRate, file.name, audio.length > n);
    }

    function recordingName() {
      const d = new win.Date();
      const p = (v) => String(v).padStart(2, "0");
      const stamp = `${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}-${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}`;
      return `tick-${stamp}-${settings.bph}.wav`;
    }

    /**
     * Show a save link for the finished recording. Browsers block a download
     * started without a click, so the click on the link starts it. The link
     * stays (after stop too) until the next recording replaces it.
     */
    function offerSave(bytes, name, secs) {
      clearSave();
      saveUrl = win.URL.createObjectURL(new win.Blob([bytes], { type: "audio/wav" }));
      el.save.setAttribute("href", saveUrl);
      el.save.download = name;
      el.save.textContent = `save recording (${secs} s, ${(bytes.length / 1e6).toFixed(1)} MB)`;
      el.save.title = `download ${name}`;
      el.save.hidden = false;
    }

    function clearSave() {
      if (!saveUrl) return;
      win.URL.revokeObjectURL(saveUrl);
      saveUrl = null;
      el.save.removeAttribute("href");
      el.save.hidden = true;
    }

    /** Open the mic; the run button reads `starting...` until it opens or fails. */
    async function startMic() {
      stop();
      const gen = startGen;
      opening = gen;
      render();
      try {
        await openMic(gen);
      } finally {
        if (opening === gen) {
          opening = -1;
          render();
        }
      }
    }

    const isOpening = () => opening === startGen;

    /** The run button: stop what runs (or is opening), else start the mic. */
    function toggleRun() {
      if (source || isOpening()) stop();
      else startMic();
    }

    async function openMic(gen) {
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

    /**
     * Status: idle, listening (no beats or no beat rate yet), measuring
     * (window filling), or locked.
     */
    function status() {
      if (!source) return analysis ? "analyzed" : "idle";
      if (!locked || !heard) return "listening";
      return settled() ? "locked" : "measuring";
    }

    /** The status in words: the empty rate graph's text, and full screen mode's. */
    function statusText() {
      if (!source && analysis) {
        const s = analysis.summary;
        return s ? `${s.beats} beats in ${analysis.name}` : `no steady beats found in ${analysis.name}`;
      }
      if (!source) return "start the mic, or simulate in the settings";
      if (!heard) return "listening for beats...";
      if (!locked) return "ticks heard, finding the beat rate...";
      if (det.doubt()) return "the beat rate stopped fitting, finding it again...";
      return settled() ? `${beatCount} beats` : progressText();
    }

    /**
     * The result the positions table would save: one fit over an analyzed
     * recording, or the latest live reading (shown, so savable, even before
     * it settles); null if neither.
     */
    function currentResult() {
      if (analysis && !source) {
        const s = analysis.summary;
        return s ? { rate: s.rate, beatError: s.beatError, bph: s.bph, settled: true } : null;
      }
      const r = readings[readings.length - 1];
      return r && locked ? { rate: r.rate, beatError: r.beatError, bph: locked.bph, settled: r.settled } : null;
    }

    function renderReadout() {
      let rate = null;
      let beatError = null;
      let settling = false;
      if (analysis && !source) {
        // one fit over the whole recording
        const s = analysis.summary;
        if (s) ({ rate, beatError } = s);
        el.bphOut.textContent = s ? `${s.bph}${s.auto ? " auto" : ""}` : "--";
        el.beatsOut.textContent = String(s ? s.beats : 0);
        const part = analysis.truncated ? `first ${MAX_ANALYZE_SECS / 60} min` : `${Math.round(analysis.duration)} s`;
        el.rateLabel.textContent = `rate over ${part} of ${analysis.name}`;
      } else {
        const r = readings[readings.length - 1];
        if (r) ({ rate, beatError } = r);
        settling = !!r && !r.settled;
        el.bphOut.textContent = locked ? `${locked.bph}${locked.auto ? " auto" : ""}` : "--";
        if (!locked && settings.bph === "auto" && source) el.bphOut.textContent = "auto";
        el.beatsOut.textContent = String(beatCount);
        el.rateLabel.textContent = "rate";
      }
      const has = rate !== null;
      el.rateOut.textContent = has ? fmtRate(rate) : "--";
      el.rateOut.dataset.state = settling ? "settling" : "";
      el.rateOut.dataset.tolerance = has ? tolerance(rate, settings.target) : "";
      el.beatErrorOut.textContent = has ? beatError.toFixed(1) : "--";
      el.beatErrorOut.dataset.level = has ? beatErrorLevel(beatError) : "";
      const state = status();
      // only a change of state is announced (the status is a live region)
      if (el.status.textContent !== state) el.status.textContent = state;
      el.status.dataset.state = state;
      const busy = !!source || isOpening();
      for (const b of [el.run, el.focusRun]) {
        b.textContent = source ? "stop" : busy ? "starting..." : "start mic";
        if (busy) b.dataset.running = "";
        else delete b.dataset.running;
      }
      el.sim.disabled = !!source;
      el.record.disabled = !!rec;
      if (rec) {
        el.record.textContent = `recording... ${Math.ceil((rec.buf.length - rec.len) / rec.sampleRate)} s`;
        el.record.dataset.recording = "";
        el.record.style.setProperty("--progress", `${((100 * rec.len) / rec.buf.length).toFixed(1)}%`);
      } else {
        el.record.textContent = `record ${RECORD_SECS} s`;
        delete el.record.dataset.recording;
        el.record.style.removeProperty("--progress");
      }
      // red while auto's choice no longer fits the beats (TICK-37)
      const doubt = !!source && !analysis && det.doubt();
      if (doubt) el.bphOut.dataset.doubt = "";
      else delete el.bphOut.dataset.doubt;
      el.bphOut.title = doubt
        ? "the beat rate stopped fitting the beats; auto is finding it again (click to choose)"
        : "choose the beat rate (b)";
      el.sessionAdd.disabled = !currentResult();
      el.sessionAdd.title = el.sessionAdd.disabled ? "no reading to save yet" : "save the current reading under this position";
      renderMeter();
      if (!el.focusView.hidden) {
        el.focusStatus.textContent = state;
        el.focusStatus.dataset.state = state;
        el.focusDetail.textContent = statusText();
        el.focusRate.textContent = el.rateOut.textContent;
        el.focusRate.dataset.state = el.rateOut.dataset.state;
        el.focusRate.dataset.tolerance = el.rateOut.dataset.tolerance;
        el.focusTrend.textContent = source ? trend(readings) : "";
        el.focusBeatError.textContent = el.beatErrorOut.textContent;
        el.focusBph.textContent = el.bphOut.textContent;
        el.focusBph.title = el.bphOut.title;
        if (doubt) el.focusBph.dataset.doubt = "";
        else delete el.focusBph.dataset.doubt;
        el.focusNote.textContent = el.meterNote.textContent;
      }
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

    /**
     * x for a time on the shared axis of the rate graph and the trace: the
     * visible span up to now while live, or the whole of an analyzed
     * recording (`fixed`).
     */
    function timeAxis(w) {
      const fixed = !!analysis && !source;
      const span = fixed ? Math.max(analysis.duration, 1) : settings.span;
      const t1 = fixed ? span : Math.max(now, span);
      const t0 = t1 - span;
      const plot = w - PAD_L - PAD_R;
      return { t0, t1, span, fixed, plot, x: (t) => PAD_L + ((t - t0) / span) * plot };
    }

    function drawTimeGrid(ctx, w, h, axis, labels) {
      const step = axis.span >= 300 ? 60 : axis.span > 20 ? 10 : 1;
      const label = (s) => (step === 60 ? `${s / 60}m` : `${s}s`);
      ctx.strokeStyle = C.grid;
      ctx.fillStyle = C.gridText;
      ctx.lineWidth = 1;
      ctx.textAlign = "center";
      for (let s = 0; s <= axis.span; s += step) {
        // live: back from now; a recording: on from its start
        const x = Math.round(axis.x(axis.fixed ? s : axis.t1 - s)) + 0.5;
        ctx.beginPath();
        ctx.moveTo(x, 0);
        ctx.lineTo(x, h - (labels ? 14 : 0));
        ctx.stroke();
        if (labels) ctx.fillText(axis.fixed ? label(s) : s === 0 ? "now" : `-${label(s)}`, x, h - 2);
      }
    }

    function drawRate() {
      const s = surface(el.rateCanvas);
      if (!s) return;
      const { ctx, w, h } = s;
      const axis = timeAxis(w);
      const plotH = h - 16;
      const visible = readings.filter((r) => r.t >= axis.t0);
      const band = TARGETS[settings.target];
      // the target band always fits, so it shows before the first reading
      const bandPeak = band ? Math.max(-band[0], band[1]) : 0;
      const range = pickRange(visible.reduce((m, r) => Math.max(m, Math.abs(r.rate)), bandPeak));
      const mid = plotH / 2;
      const half = mid - 8;
      const y = (v) => mid - (v / range) * half;

      ctx.fillStyle = C.fast;
      ctx.fillRect(PAD_L, mid - half, w - PAD_L - PAD_R, half);
      ctx.fillStyle = C.slow;
      ctx.fillRect(PAD_L, mid, w - PAD_L - PAD_R, half);
      if (band) {
        ctx.fillStyle = C.band;
        ctx.fillRect(PAD_L, y(band[1]), w - PAD_L - PAD_R, y(band[0]) - y(band[1]));
        ctx.strokeStyle = C.bandEdge;
        ctx.lineWidth = 1;
        ctx.setLineDash([2, 3]);
        for (const v of band) {
          ctx.beginPath();
          ctx.moveTo(PAD_L, Math.round(y(v)) + 0.5);
          ctx.lineTo(w - PAD_R, Math.round(y(v)) + 0.5);
          ctx.stroke();
        }
        ctx.setLineDash([]);
      }
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
        ctx.fillText(statusText(), cx, mid - 16);
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
      drawCursor(ctx, axis, plotH, visible, (r) => y(r.rate), (r) => `${fmtRate(r.rate)} s/d`);
    }

    /**
     * The pointer's reading (TICK-31): a line through the item nearest the
     * pointer's time, a dot on it, and its value and time.
     */
    function drawCursor(ctx, axis, h, items, yOf, label) {
      if (!cursor || !items.length) return;
      const t = axis.t0 + cursor.frac * axis.span;
      let best = items[0];
      for (const it of items) if (Math.abs(it.t - t) < Math.abs(best.t - t)) best = it;
      const x = axis.x(best.t);
      ctx.globalAlpha = 0.6;
      ctx.strokeStyle = C.cursor;
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(Math.round(x) + 0.5, 0);
      ctx.lineTo(Math.round(x) + 0.5, h);
      ctx.stroke();
      ctx.globalAlpha = 1;
      ctx.fillStyle = C.cursor;
      ctx.beginPath();
      ctx.arc(x, yOf(best), 3.5, 0, Math.PI * 2);
      ctx.fill();
      const when = axis.fixed ? `${best.t.toFixed(1)}s` : `-${(axis.t1 - best.t).toFixed(1)}s`;
      // the label sits on the side with more room
      const right = x > PAD_L + axis.plot / 2;
      ctx.textAlign = right ? "right" : "left";
      ctx.textBaseline = "top";
      ctx.fillText(`${label(best)} at ${when}`, x + (right ? -6 : 6), 4);
    }

    /** Point the cursor at a pointer event over a graph; null clears it. */
    function pointAt(canvas, e) {
      if (!e) {
        cursor = null;
      } else {
        const w = canvas.clientWidth || baseSize.get(canvas).w;
        const px = e.clientX - canvas.getBoundingClientRect().left;
        cursor = { frac: Math.max(0, Math.min(1, (px - PAD_L) / (w - PAD_L - PAD_R))) };
      }
      schedule();
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
      drawCursor(ctx, axis, h, visible, (b) => y(b.offsetMs), (b) => `${b.parity ? "tock" : "tick"} ${signed(b.offsetMs, 2)} ms`);
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
    // info dialogs (TICK-26)
    // ---------------------------------------------------------------------

    function openInfo(key) {
      const info = INFO[key];
      el.infoTitle.textContent = info.title;
      el.infoBody.replaceChildren(
        ...info.body.map((text) => {
          const p = doc.createElement("p");
          p.textContent = text;
          return p;
        }),
      );
      if (el.info.showModal) el.info.showModal();
      else el.info.setAttribute("open", "");
      el.infoClose.focus();
    }

    function closeInfo() {
      if (el.info.close) el.info.close();
      else el.info.removeAttribute("open");
    }

    // ---------------------------------------------------------------------
    // beat rate picker (TICK-37)
    // ---------------------------------------------------------------------

    /** Choose the beat rate from the readout: auto, a rate, or hold auto's choice. */
    function openBph() {
      const held = settings.bph === "auto" && locked && locked.auto ? locked.bph : null;
      el.bphNow.textContent =
        settings.bph !== "auto"
          ? `set to ${settings.bph} bph; it stays until you pick another or auto`
          : !source || !det
            ? "auto: found from the gaps between beats"
            : det.doubt()
              ? `auto: ${locked.bph} stopped fitting the beats, finding the rate again`
              : held
                ? `auto chose ${held} bph; hold it to stop auto changing it`
                : "auto: finding the beat rate";
      const choice = (label, value, pressed) => {
        const b = doc.createElement("button");
        b.textContent = label;
        b.dataset.bph = String(value);
        b.setAttribute("aria-pressed", String(pressed));
        b.addEventListener("click", () => pickBph(value));
        return b;
      };
      const choices = [...el.bph.options].map((o) => {
        const value = o.value === "auto" ? "auto" : Number(o.value);
        return choice(o.textContent, value, value === settings.bph);
      });
      if (held) {
        const hold = choice(`hold ${held}`, held, false);
        hold.className = "primary";
        hold.id = "bph-hold";
        choices.unshift(hold);
      }
      el.bphChoices.replaceChildren(...choices);
      if (el.bphDialog.showModal) el.bphDialog.showModal();
      else el.bphDialog.setAttribute("open", "");
      (held ? choices[0] : el.bphClose).focus();
    }

    function closeBph() {
      if (el.bphDialog.close) el.bphDialog.close();
      else el.bphDialog.removeAttribute("open");
    }

    function pickBph(value) {
      closeBph();
      el.bph.value = String(value);
      onSetting("bph", value);
    }

    // ---------------------------------------------------------------------
    // full screen mode (TICK-28)
    // ---------------------------------------------------------------------

    function openFocus() {
      // the same settings, folded under their toggle (TICK-28)
      settingsWasOpen = el.settingsBox.open;
      el.settingsBox.open = false;
      el.focusSettings.append(el.settingsBox);
      el.focusView.hidden = false;
      const fs = el.focusView.requestFullscreen;
      if (fs) {
        focusFull = true;
        // refused (no user gesture, or not allowed): it still covers the page
        Promise.resolve(fs.call(el.focusView)).catch(() => (focusFull = false));
      }
      el.focusExit.focus();
      render();
    }

    function closeFocus() {
      settingsHome.append(el.settingsBox);
      el.settingsBox.open = settingsWasOpen;
      el.focusView.hidden = true;
      fitSettings();
      if (focusFull && doc.fullscreenElement && doc.exitFullscreen) doc.exitFullscreen();
      focusFull = false;
      el.focus.focus();
    }

    const toggleFocus = () => (el.focusView.hidden ? openFocus() : closeFocus());

    // ---------------------------------------------------------------------
    // positions (TICK-29)
    // ---------------------------------------------------------------------

    function renderSession() {
      const cell = (text, attr, value) => {
        const td = doc.createElement("td");
        td.textContent = text;
        if (attr && value) td.dataset[attr] = value;
        return td;
      };
      el.sessionBody.replaceChildren(
        ...session.map((r) => {
          const tr = doc.createElement("tr");
          const th = doc.createElement("th");
          th.textContent = POSITIONS[r.pos];
          const remove = doc.createElement("button");
          remove.className = "remove";
          remove.textContent = "remove";
          remove.setAttribute("aria-label", `remove ${POSITIONS[r.pos]}`);
          remove.addEventListener("click", () => removePosition(r.pos));
          const last = doc.createElement("td");
          last.append(remove);
          tr.append(
            th,
            cell(fmtRate(r.rate), "tolerance", tolerance(r.rate, settings.target)),
            cell(r.beatError.toFixed(1), "level", beatErrorLevel(r.beatError)),
            cell(String(r.bph)),
            last,
          );
          return tr;
        }),
      );
      const s = sessionStats(session);
      el.sessionAvg.textContent = s ? fmtRate(s.rate) : "--";
      el.sessionAvgBe.textContent = s ? s.beatError.toFixed(1) : "--";
      el.sessionDelta.textContent = s ? s.delta.toFixed(1) : "--";
      el.sessionCopy.disabled = !session.length;
      el.sessionClear.disabled = !session.length;
    }

    /** Save the current result under the chosen position, then pick the next free one. */
    function addPosition() {
      const cur = currentResult();
      if (!cur) return;
      const { settled: full, ...result } = cur;
      const pos = el.sessionPos.value;
      session = sortSession([...session.filter((r) => r.pos !== pos), { pos, ...result }]);
      saveSession();
      const order = POSITION_ORDER;
      const next = [...order.slice(order.indexOf(pos) + 1), ...order].find((p) => !session.some((r) => r.pos === p));
      if (next) el.sessionPos.value = next;
      el.sessionNote.textContent = `saved ${POSITIONS[pos]}${full ? "" : " (before the averaging window filled)"}`;
      renderSession();
    }

    function removePosition(pos) {
      session = session.filter((r) => r.pos !== pos);
      saveSession();
      el.sessionNote.textContent = "";
      renderSession();
    }

    function clearSession() {
      session = [];
      saveSession();
      el.sessionNote.textContent = "";
      renderSession();
    }

    async function copySession() {
      const clip = win.navigator.clipboard;
      try {
        await clip.writeText(sessionText(session));
        el.sessionNote.textContent = "copied";
      } catch {
        el.sessionNote.textContent = "copy failed: the browser blocked the clipboard";
      }
    }

    // ---------------------------------------------------------------------
    // keys (TICK-34) and dropped files (TICK-32)
    // ---------------------------------------------------------------------

    function onKey(e) {
      if (e.key === "Escape") {
        // a native modal closes on escape by itself; this covers the fallback
        if (el.info.hasAttribute("open")) closeInfo();
        else if (el.bphDialog.hasAttribute("open")) closeBph();
        else if (!el.focusView.hidden) closeFocus();
        return;
      }
      if (e.ctrlKey || e.metaKey || e.altKey) return;
      if (el.info.hasAttribute("open") || el.bphDialog.hasAttribute("open")) return;
      const t = e.target;
      if (t && t.closest && t.closest("input, select, textarea, summary")) return;
      if (e.key === " ") {
        // instead of pressing whichever button has focus
        e.preventDefault();
        toggleRun();
      } else if (e.key === "r") {
        startRecording();
      } else if (e.key === "f") {
        toggleFocus();
      } else if (e.key === "b") {
        openBph();
      }
    }

    const hasFiles = (e) => !!e.dataTransfer && [...e.dataTransfer.types].includes("Files");

    function onDrag(e) {
      if (!hasFiles(e)) return;
      // without this the browser opens the file in place of the page
      e.preventDefault();
      if (e.type === "dragenter") dragDepth++;
      else if (e.type === "dragleave") dragDepth = Math.max(0, dragDepth - 1);
      else if (e.type === "drop") dragDepth = 0;
      el.drop.hidden = dragDepth === 0;
      if (e.type === "drop" && e.dataTransfer.files.length) analyzeFile(e.dataTransfer.files[0]);
    }

    /** Fold the settings away on a narrow screen, and open them on a wide one. */
    function fitSettings() {
      // full screen mode keeps them folded (entering fullscreen resizes)
      if (!win.matchMedia || !el.focusView.hidden) return;
      const narrow = win.matchMedia(NARROW).matches;
      if (!narrow) el.settingsBox.open = true;
      return narrow;
    }

    // ---------------------------------------------------------------------
    // wiring
    // ---------------------------------------------------------------------

    function onSetting(key, value) {
      settings[key] = value;
      settings = sanitize(settings);
      saveSettings();
      if (key === "span" || key === "target") {
        // display only: the readings stand
        if (key === "target") renderSession();
        render();
        return;
      }
      if (!source && analysis) {
        // look at the same recording with the new settings
        reanalyze();
        return;
      }
      if (det) {
        if (key === "bph") {
          // a new rate locks (and clears the beats) unless it holds auto's choice
          for (const ev of det.setBph(settings.bph)) handle(ev);
          if (settings.bph === "auto") {
            locked = null;
            clearData();
          }
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
      el.run.addEventListener("click", toggleRun);
      el.focusRun.addEventListener("click", toggleRun);
      el.sim.addEventListener("click", startSim);
      el.record.addEventListener("click", startRecording);
      el.open.addEventListener("click", () => el.file.click());
      for (const b of doc.querySelectorAll("[data-info]")) {
        b.addEventListener("click", () => openInfo(b.dataset.info));
      }
      el.infoClose.addEventListener("click", closeInfo);
      // a click on the backdrop lands on the dialog itself, not its content
      el.info.addEventListener("click", (e) => {
        if (e.target === el.info) closeInfo();
      });
      el.bphOut.addEventListener("click", openBph);
      el.focusBph.addEventListener("click", openBph);
      el.bphClose.addEventListener("click", closeBph);
      el.bphDialog.addEventListener("click", (e) => {
        if (e.target === el.bphDialog) closeBph();
      });
      doc.addEventListener("keydown", onKey);
      for (const type of ["dragenter", "dragover", "dragleave", "drop"]) doc.addEventListener(type, onDrag);
      el.focus.addEventListener("click", openFocus);
      el.focusExit.addEventListener("click", closeFocus);
      // leaving fullscreen (escape, the browser's own control) leaves full screen mode
      doc.addEventListener("fullscreenchange", () => {
        if (!doc.fullscreenElement && focusFull && !el.focusView.hidden) closeFocus();
      });
      doc.addEventListener("visibilitychange", () => {
        if (doc.visibilityState === "visible") holdWake();
      });
      el.sessionAdd.addEventListener("click", addPosition);
      el.sessionCopy.addEventListener("click", copySession);
      el.sessionClear.addEventListener("click", clearSession);
      for (const canvas of [el.rateCanvas, el.traceCanvas]) {
        canvas.classList.add("pointable");
        canvas.addEventListener("pointermove", (e) => pointAt(canvas, e));
        canvas.addEventListener("pointerdown", (e) => pointAt(canvas, e));
        // a finger lifting off leaves the reading up until the next tap
        canvas.addEventListener("pointerleave", (e) => {
          if (e.pointerType !== "touch") pointAt(canvas, null);
        });
      }
      if (fitSettings()) el.settingsBox.open = false;
      el.file.addEventListener("change", () => {
        const file = el.file.files && el.file.files[0];
        // cleared so picking the same file again still fires a change
        el.file.value = "";
        if (file) analyzeFile(file);
      });
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
      el.target.addEventListener("change", () => onSetting("target", el.target.value));
      win.addEventListener("resize", () => {
        fitSettings();
        schedule();
      });
      renderSession();
      render();
    }

    return {
      start,
      stop,
      startMic,
      startSim,
      record: startRecording,
      analyzeFile,
      feed,
      // for tests
      state: () => ({ settings, readings, beats, waves, beatCount, locked, running: !!source, analysis, session, cursor, wake }),
    };
  }

  if (typeof module === "object" && module.exports) {
    module.exports = {
      createTick,
      sanitize,
      sanitizeSession,
      pickRange,
      fmtRate,
      toDb,
      meterPct,
      encodeWav,
      tolerance,
      beatErrorLevel,
      trend,
      sessionStats,
      sessionText,
      DEFAULTS,
      INFO,
    };
  } else {
    createTick(root).start();
  }
})(typeof window === "undefined" ? globalThis : window);
