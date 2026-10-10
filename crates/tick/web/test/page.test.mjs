// The tick page under jsdom (spec/tick.md TICK-3 to TICK-6, TICK-13 to
// TICK-19).
import { test } from "node:test";
import assert from "node:assert/strict";
import { boot, fakeAudio, settle, texts, callsOf, sets, tick, D } from "../helpers.mjs";

const rateOf = (p) => Number(p.$("rate").textContent);

test("boots idle with defaults and an empty graph", () => {
  const p = boot();
  assert.equal(p.$("status").textContent, "idle");
  assert.equal(p.$("status").dataset.state, "idle");
  assert.equal(p.$("rate").textContent, "--");
  assert.equal(p.$("beat-error").textContent, "--");
  assert.equal(p.$("bph").textContent, "--");
  assert.equal(p.$("beats").textContent, "0");
  assert.equal(p.$("start").disabled, false);
  assert.equal(p.$("stop").disabled, true);
  assert.equal(p.$("set-bph").value, "auto");
  assert.equal(p.$("set-average").value, "10");
  assert.equal(p.$("set-span").value, "60");
  assert.equal(p.$("set-correction").value, "0");
  assert.equal(p.$("set-sensitivity").value, "normal");
  const rate = texts(p.ctx("rate-canvas"));
  assert.ok(rate.includes("start the mic or simulate"));
  // zero in the middle, symmetric labels, fast up and slow down
  for (const label of ["0", "+5", "-5", "+2.5", "-2.5", "fast", "slow", "now", "-60s"]) {
    assert.ok(rate.includes(label), `rate graph labels ${label}`);
  }
  assert.deepEqual(texts(p.ctx("scope-canvas")), ["0", "10", "20", "30"]);
});

test("settings load from storage and fall back to defaults", () => {
  const p = boot({ storage: JSON.stringify({ bph: 21600, average: 30, span: 300, correction: -2.5 }) });
  assert.equal(p.$("set-bph").value, "21600");
  assert.equal(p.$("set-average").value, "30");
  assert.equal(p.$("set-span").value, "300");
  assert.equal(p.$("set-correction").value, "-2.5");
  assert.ok(texts(p.ctx("rate-canvas")).includes("-5m"));

  const bad = boot({ storage: "{not json" });
  assert.equal(bad.$("set-bph").value, "auto");

  const blocked = boot({ storage: "throw" });
  assert.equal(blocked.$("set-average").value, "10");
  // saving into a blocked storage is a no-op
  blocked.change("set-average", "4");
  assert.equal(blocked.app.state().settings.average, 4);
});

test("sanitize rejects values outside the allowed sets", () => {
  assert.deepEqual(tick.sanitize(null), tick.DEFAULTS);
  assert.deepEqual(tick.sanitize({ bph: 1234, average: 7, span: 5, correction: "x" }), tick.DEFAULTS);
  assert.equal(tick.sanitize({ correction: 1e9 }).correction, 100);
  assert.equal(tick.sanitize({ correction: -1e9 }).correction, -100);
  assert.equal(tick.sanitize({ correction: Infinity }).correction, 0);
  assert.equal(tick.sanitize({ sensitivity: "loud" }).sensitivity, "normal");
  assert.equal(tick.sanitize({ sensitivity: "max" }).sensitivity, "max");
});

test("toDb and meterPct map levels onto the meter", () => {
  assert.equal(tick.toDb(0), -100);
  assert.equal(tick.toDb(1e-9), -100);
  assert.equal(tick.toDb(1), 0);
  assert.ok(Math.abs(tick.toDb(0.01) + 40) < 1e-9);
  assert.equal(tick.meterPct(0), 0);
  assert.equal(tick.meterPct(1), 100);
  assert.ok(Math.abs(tick.meterPct(0.01) - 60) < 1e-9);
});

test("the input meter shows level, noise floor, and threshold while running", async () => {
  const p = boot();
  assert.equal(p.$("meter-db").textContent, "--");
  const audio = fakeAudio(p.win);
  await p.app.startMic();
  audio.play({ bph: 28800, amp: 0.05, noise: 0.002 }, 3);
  p.flush();
  const db = Number(p.$("meter-db").textContent.replace(" dB", ""));
  assert.ok(db > -60 && db < 0, `${db} dB`);
  const pct = (id) => parseFloat(p.$(id).style.left);
  assert.ok(pct("meter-threshold") > pct("meter-noise"), "threshold sits over the noise floor");
  assert.ok(parseFloat(p.$("meter-level").style.width) > pct("meter-threshold"), "ticks reach past it");
  assert.equal(p.$("meter-note").textContent, "");
  p.click("stop");
  assert.equal(p.$("meter-db").textContent, "--");
  assert.equal(p.$("meter-level").style.width, "0%");
});

test("the meter says when the microphone sends only silence", async () => {
  const p = boot();
  const audio = fakeAudio(p.win);
  await p.app.startMic();
  for (let i = 0; i < 70; i++) {
    audio.play({ amp: 0, noise: 0 }, 0.04);
    p.flush();
  }
  assert.match(p.$("meter-note").textContent, /No audio from the microphone/);
  assert.equal(p.$("meter-db").textContent, "-100 dB");
});

test("the meter counts loud sounds it ignored", async () => {
  const p = boot();
  const audio = fakeAudio(p.win);
  await p.app.startMic();
  audio.play({ bph: 28800, amp: 0.01, noise: 0.001 }, 4);
  const knock = D.synth({ sampleRate: 48000, bph: 12000, start: 0, noise: 0, amp: 0.8 }).next(2048);
  for (let i = 0; i < 2; i++) {
    p.app.feed(knock);
    audio.play({ bph: 28800, amp: 0, noise: 0.001, seed: 4 + i }, 0.5);
  }
  p.flush();
  assert.match(p.$("meter-note").textContent, /^[12] loud sounds? ignored$/);
});

test("sensitivity applies to a running source", async () => {
  const p = boot();
  const audio = fakeAudio(p.win);
  await p.app.startMic();
  // faint ticks: unheard at low, read at max
  p.change("set-sensitivity", "low");
  audio.play({ bph: 28800, amp: 0.008, noise: 0.004 }, 5);
  assert.equal(p.app.state().beatCount, 0);
  p.change("set-sensitivity", "max");
  audio.play({ bph: 28800, amp: 0.008, noise: 0.004, seed: 3 }, 5);
  assert.ok(p.app.state().beatCount > 10);
});

test("pickRange and fmtRate", () => {
  assert.equal(tick.pickRange(0), 5);
  assert.equal(tick.pickRange(4.3), 5);
  assert.equal(tick.pickRange(4.4), 10);
  assert.equal(tick.pickRange(250), 300);
  assert.equal(tick.pickRange(5000), 600);
  assert.equal(tick.fmtRate(6.04), "+6.0");
  assert.equal(tick.fmtRate(-0.04), "0.0");
  assert.equal(tick.fmtRate(-12.36), "-12.4");
});

test("settings changes persist", () => {
  const p = boot();
  p.change("set-bph", "28800");
  p.change("set-average", "60");
  p.change("set-span", "30");
  p.change("set-correction", "1.5");
  p.change("set-sensitivity", "high");
  const saved = JSON.parse(p.win.localStorage.getItem("tick.settings"));
  assert.deepEqual(saved, { bph: 28800, average: 60, span: 30, correction: 1.5, sensitivity: "high" });
  assert.equal(boot({ storage: JSON.stringify(saved) }).$("set-sensitivity").value, "high");
  p.change("set-correction", "abc");
  assert.equal(p.$("set-correction").value, "0");
  p.change("set-bph", "auto");
  assert.equal(JSON.parse(p.win.localStorage.getItem("tick.settings")).bph, "auto");
});

test("simulate locks on and reads the simulated watch", () => {
  const p = boot();
  p.click("sim");
  assert.equal(p.$("status").textContent, "listening");
  assert.equal(p.$("sim").disabled, true);
  assert.equal(p.$("stop").disabled, false);
  assert.equal(p.intervals.size, 1);
  assert.ok(texts(p.ctx("rate-canvas")).includes("listening for beats..."));
  // ticks heard, rate not yet known
  p.advance(600);
  p.flush();
  assert.ok(p.app.state().beatCount === 0 && p.app.state().waves.length > 0);
  assert.ok(texts(p.ctx("rate-canvas")).includes("ticks heard, finding the beat rate..."));
  // locked, readings coming in while the 10 s window fills (TICK-23)
  p.advance(5400);
  p.flush();
  assert.equal(p.$("status").textContent, "measuring");
  assert.equal(p.$("rate").dataset.state, "settling");
  const filling = texts(p.ctx("rate-canvas")).find((t) => t.startsWith("measuring: "));
  assert.match(filling, /^measuring: [3-5]\.[0-9] of 10 s$/);
  assert.ok(callsOf(p.ctx("rate-canvas"), "setLineDash").some((c) => c[1].length === 2), "dashed while settling");
  // window full: settled, solid
  p.advance(8000);
  p.flush();
  assert.equal(p.$("status").textContent, "locked");
  assert.equal(p.$("rate").dataset.state, "");
  assert.ok(!texts(p.ctx("rate-canvas")).some((t) => t.startsWith("measuring: ")));
  assert.equal(p.$("bph").textContent, "28800 auto");
  assert.ok(Math.abs(rateOf(p) - 6) < 0.5, p.$("rate").textContent);
  assert.ok(Math.abs(Number(p.$("beat-error").textContent) - 0.4) <= 0.1);
  assert.ok(Number(p.$("beats").textContent) > 30);

  const rate = p.ctx("rate-canvas");
  assert.equal(callsOf(rate, "arc").length > 0, true, "head dot");
  assert.ok(texts(rate).some((t) => /^\+[0-9]\.[0-9]$/.test(t)), "head label");
  const trace = p.ctx("trace-canvas");
  const dots = callsOf(trace, "fillRect").length;
  assert.ok(dots > 30, `${dots} trace dots`);
  assert.ok(sets(trace, "fillStyle").includes("#ffb547"));
  assert.ok(sets(trace, "fillStyle").includes("#4fd1c5"));
  assert.ok(texts(trace).includes("ms"));
  const scope = p.ctx("scope-canvas");
  assert.ok(callsOf(scope, "lineTo").length > 100);
  // oldest faint, newest opaque
  const alphas = sets(scope, "globalAlpha");
  assert.equal(alphas.length, 17);
  assert.equal(alphas[15], 1);
  assert.ok(alphas[0] < 0.25);
  assert.equal(p.app.state().waves.length, 16);

  // a long run trims history past the longest span
  p.advance(310000, 1000);
  p.flush();
  const { readings } = p.app.state();
  assert.ok(readings[readings.length - 1].t - readings[0].t <= 301);

  p.click("stop");
  assert.equal(p.intervals.size, 0);
  assert.equal(p.$("status").textContent, "idle");
  // the last reading stays up after stopping
  assert.notEqual(p.$("rate").textContent, "--");
  assert.ok(texts(p.ctx("rate-canvas")).every((t) => t !== "start the mic or simulate"));
});

test("changing the beat rate while running relocks and clears the graphs", () => {
  const p = boot();
  p.click("sim");
  p.advance(4000);
  p.flush();
  assert.ok(p.app.state().readings.length > 0);
  p.change("set-bph", "21600");
  assert.equal(p.$("bph").textContent, "21600");
  assert.equal(p.app.state().readings.length, 0);
  assert.ok(texts(p.ctx("rate-canvas")).includes("measuring: 0.0 of 10 s"));
  assert.ok(callsOf(p.ctx("rate-canvas"), "fillRect").length > 2, "progress bar");
  p.change("set-bph", "auto");
  assert.equal(p.$("bph").textContent, "--");
  assert.equal(p.$("status").textContent, "listening");
  p.advance(4000);
  p.flush();
  assert.equal(p.$("bph").textContent, "28800 auto");
});

test("a fixed beat rate is locked from the start", () => {
  const p = boot({ storage: JSON.stringify({ bph: 28800 }) });
  p.click("sim");
  assert.equal(p.$("bph").textContent, "28800");
  assert.equal(p.$("status").textContent, "measuring");
  // nothing heard yet, even with the rate known
  assert.ok(texts(p.ctx("rate-canvas")).includes("listening for beats..."));
});

test("progress before the first reading counts from the beats since the lock", () => {
  const p = boot({ storage: JSON.stringify({ bph: 28800, average: 30 }) });
  p.click("sim");
  // beats indexed but under the 1 s a reading needs
  p.advance(900);
  p.flush();
  assert.equal(p.app.state().readings.length, 0);
  const msg = texts(p.ctx("rate-canvas")).find((t) => t.startsWith("measuring: "));
  assert.match(msg, /^measuring: 0\.[1-9] of 30 s$/);
});

test("averaging and correction apply to a running source", () => {
  const p = boot();
  p.click("sim");
  p.advance(5000);
  p.change("set-average", "4");
  p.change("set-correction", "-6");
  p.advance(5000);
  p.flush();
  assert.ok(Math.abs(rateOf(p)) < 0.5, p.$("rate").textContent);
});

test("a backgrounded tab catches up at most a second at a time", () => {
  const p = boot();
  p.click("sim");
  p.advance(30000, 30000);
  assert.ok(Math.abs(p.app.state().waves.length - 0) <= 16);
  // a second of audio is 8 beats at most
  assert.ok(p.app.state().beatCount <= 8);
});

test("the mic opens with processing off and reads through the worklet", async () => {
  const p = boot();
  const audio = fakeAudio(p.win);
  p.click("start");
  await settle();
  assert.deepEqual(audio.log.constraints, {
    audio: { echoCancellation: false, noiseSuppression: false, autoGainControl: false, channelCount: 1 },
    video: false,
  });
  assert.deepEqual(audio.log.modules, ["/tick/static/worklet.js"]);
  assert.equal(audio.log.processor, "tick-capture");
  assert.deepEqual(audio.log.connected, [
    ["input", "worklet"],
    ["worklet", "mute"],
    ["mute", "destination"],
  ]);
  assert.equal(p.$("status").textContent, "listening");
  audio.play({ bph: 21600, rate: -15, beatError: 0.6 }, 8);
  p.flush();
  assert.equal(p.$("bph").textContent, "21600 auto");
  assert.ok(Math.abs(rateOf(p) + 15) < 0.5, p.$("rate").textContent);
  p.click("stop");
  assert.equal(audio.log.tracksStopped, 1);
  assert.equal(audio.log.closed, 1);
});

test("the mic falls back to a ScriptProcessor", async () => {
  const p = boot({ raf: false });
  const audio = fakeAudio(p.win, { worklet: false });
  await p.app.startMic();
  assert.equal(audio.log.blockSize, 2048);
  audio.play({ bph: 28800, rate: 3 }, 6);
  p.flush();
  assert.ok(Math.abs(rateOf(p) - 3) < 0.5);
});

test("microphone errors show a message and stay idle", async () => {
  const p = boot();
  await p.app.startMic();
  assert.match(p.$("error").textContent, /secure \(https\)/);
  assert.equal(p.$("error").hidden, false);

  fakeAudio(p.win, { deny: Object.assign(new Error("no"), { name: "NotAllowedError" }) });
  await p.app.startMic();
  assert.equal(p.$("error").textContent, "Microphone permission was denied.");

  fakeAudio(p.win, { deny: new Error("busy") });
  await p.app.startMic();
  assert.equal(p.$("error").textContent, "Could not open the microphone: busy");

  fakeAudio(p.win, { deny: "odd" });
  await p.app.startMic();
  assert.equal(p.$("error").textContent, "Could not open the microphone: odd");

  const audio = fakeAudio(p.win, { addModule: () => Promise.reject(new Error("blocked")) });
  await p.app.startMic();
  assert.equal(p.$("error").textContent, "Audio setup failed: blocked");
  assert.equal(audio.log.tracksStopped, 1);
  assert.equal(audio.log.closed, 1);
  assert.equal(p.$("status").textContent, "idle");

  // a working start clears the error
  fakeAudio(p.win);
  await p.app.startMic();
  assert.equal(p.$("error").hidden, true);
});

test("an audio setup failure before the context exists still releases the mic", async () => {
  const p = boot();
  const audio = fakeAudio(p.win);
  p.win.AudioContext = class {
    constructor() {
      throw "no audio";
    }
  };
  await p.app.startMic();
  assert.equal(p.$("error").textContent, "Audio setup failed: no audio");
  assert.equal(audio.log.tracksStopped, 1);
});

test("stopping while the mic is opening releases it", async () => {
  const p = boot();
  let grant;
  const audio = fakeAudio(p.win, { gum: () => new Promise((r) => (grant = r)) });
  const opening = p.app.startMic();
  p.click("stop");
  grant({ getTracks: () => [{ stop: () => audio.log.tracksStopped++ }] });
  await opening;
  assert.equal(audio.log.tracksStopped, 1);
  assert.equal(p.app.state().running, false);

  // and while the worklet loads
  let loaded;
  const audio2 = fakeAudio(p.win, { addModule: () => new Promise((r) => (loaded = r)) });
  const opening2 = p.app.startMic();
  await settle();
  p.app.stop();
  loaded();
  await opening2;
  assert.equal(audio2.log.tracksStopped, 1);
  assert.equal(audio2.log.closed, 1);
  assert.equal(p.app.state().running, false);
});

test("audio arriving after stop is ignored", () => {
  const p = boot();
  p.app.feed(new Float32Array(2048));
  assert.equal(p.app.state().beatCount, 0);
});

test("canvases size to device pixel ratio and survive a missing 2d context", () => {
  const p = boot({ dpr: 2 });
  const c = p.$("rate-canvas");
  assert.equal(c.width, 1800);
  assert.equal(c.height, 560);
  assert.deepEqual(callsOf(p.ctx("rate-canvas"), "setTransform")[0], ["setTransform", 2, 0, 0, 2, 0, 0]);

  const none = boot({ canvas: false });
  none.click("sim");
  none.advance(3000);
  none.flush();
  assert.equal(none.$("status").textContent, "measuring");
});

test("resizing redraws on the next frame", () => {
  const p = boot();
  const before = p.ctx("rate-canvas").calls.length;
  p.win.dispatchEvent(new p.win.Event("resize"));
  p.win.dispatchEvent(new p.win.Event("resize"));
  p.flush();
  const after = p.ctx("rate-canvas").calls.length;
  assert.ok(after > before);
  // two resizes, one frame
  assert.equal(p.ctx("rate-canvas").calls.filter((c) => c[0] === "setTransform").length, 2);
});

test("a slow watch draws below zero and labels the head under the line", () => {
  const p = boot();
  const audio = fakeAudio(p.win);
  return p.app.startMic().then(() => {
    audio.play({ bph: 28800, rate: -40 }, 6);
    p.flush();
    const rate = p.ctx("rate-canvas");
    // range grows to hold -40 with margin
    assert.ok(texts(rate).includes("-60"));
    const baselines = sets(rate, "textBaseline");
    assert.equal(baselines[baselines.length - 1], "bottom");
  });
});

test("silent waves leave the scope blank", () => {
  const p = boot();
  p.click("sim");
  const { waves } = p.app.state();
  waves.push({ wave: new Float32Array(10), parity: null });
  p.app.feed(new Float32Array(0));
  p.flush();
  assert.deepEqual(sets(p.ctx("scope-canvas"), "globalAlpha"), []);
});
