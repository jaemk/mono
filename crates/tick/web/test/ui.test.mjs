// The tick page's working aids under jsdom (spec/tick.md TICK-27 to TICK-35):
// wake lock, big readout, positions, target band, graph cursor, dropped
// files, folding settings, keys, and the status live region.
import { test } from "node:test";
import assert from "node:assert/strict";
import { boot, fakeAudio, settle, texts, sets, tick, D } from "../helpers.mjs";

/** A screen wake lock whose requests resolve, or reject with `deny`. */
function fakeWakeLock(win, { deny, hold } = {}) {
  const log = { requests: 0, released: 0, sentinels: [], resolve: [] };
  Object.defineProperty(win.navigator, "wakeLock", {
    configurable: true,
    value: {
      request(type) {
        assert.equal(type, "screen");
        log.requests++;
        if (deny) return Promise.reject(new Error("denied"));
        const listeners = [];
        const sentinel = {
          release: () => log.released++,
          addEventListener: (t, fn) => listeners.push(fn),
          // the browser letting go, as when the tab is hidden
          lose: () => listeners.forEach((fn) => fn()),
        };
        log.sentinels.push(sentinel);
        if (hold) return new Promise((r) => log.resolve.push(() => r(sentinel)));
        return Promise.resolve(sentinel);
      },
    },
  });
  return log;
}

/** Run the simulator until its reading has settled (status locked). */
function settledSim(p, ms = 16000) {
  p.click("sim");
  p.advance(ms);
  p.flush();
  assert.equal(p.$("status").textContent, "locked");
}

const key = (p, k, opts = {}, target = p.win.document) =>
  target.dispatchEvent(new p.win.KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true, ...opts }));

test("tolerance, beat error levels, and trends", () => {
  assert.equal(tick.tolerance(5, "off"), "");
  assert.equal(tick.tolerance(-4, "cosc"), "in");
  assert.equal(tick.tolerance(6, "cosc"), "in");
  assert.equal(tick.tolerance(6.1, "cosc"), "out");
  assert.equal(tick.tolerance(-4.1, "cosc"), "out");
  assert.equal(tick.tolerance(-20, "20"), "in");
  assert.equal(tick.tolerance(30.5, "30"), "out");
  assert.equal(tick.beatErrorLevel(1), "");
  assert.equal(tick.beatErrorLevel(1.1), "warn");
  assert.equal(tick.beatErrorLevel(3), "warn");
  assert.equal(tick.beatErrorLevel(3.1), "bad");

  const at = (pairs) => pairs.map(([t, rate]) => ({ t, rate }));
  assert.equal(tick.trend([]), "");
  // nothing 5 s old yet
  assert.equal(tick.trend(at([[0, 1], [4.9, 9]])), "");
  assert.equal(tick.trend(at([[0, 1], [5, 9]])), "↑");
  assert.equal(tick.trend(at([[0, 9], [3, 0], [6, 1]])), "↓");
  // the last reading at least 5 s back is the one compared
  assert.equal(tick.trend(at([[0, 50], [2, 10], [7, 10.5]])), "→");
});

test("position stats, text, and stored rows", () => {
  assert.equal(tick.sessionStats([]), null);
  const rows = [
    { pos: "DU", rate: 4, beatError: 0.4, bph: 28800 },
    { pos: "CD", rate: -2, beatError: 0.6, bph: 28800 },
  ];
  assert.deepEqual(tick.sessionStats(rows), { rate: 1, beatError: 0.5, delta: 6 });
  assert.equal(
    tick.sessionText(rows),
    [
      "tick positions",
      "DU  dial up         +4.0 s/d   0.4 ms  28800 bph",
      "CD  crown down      -2.0 s/d   0.6 ms  28800 bph",
      "average +1.0 s/d, 0.5 ms",
      "delta 6.0 s/d",
    ].join("\n"),
  );
  assert.equal(tick.sessionText([]), "tick positions");

  assert.deepEqual(tick.sanitizeSession(null), []);
  assert.deepEqual(tick.sanitizeSession({}), []);
  const clean = tick.sanitizeSession([
    { pos: "CR", rate: 1, beatError: 0.1, bph: 18000 },
    null,
    { pos: "XX", rate: 1, beatError: 0.1, bph: 18000 },
    { pos: "DU", rate: "1", beatError: 0.1, bph: 18000 },
    { pos: "DU", rate: 2, beatError: Infinity, bph: 18000 },
    { pos: "DU", rate: 3, beatError: 0.2, bph: 18000 },
    // a second row for a position is dropped
    { pos: "DU", rate: 4, beatError: 0.2, bph: 18000 },
  ]);
  assert.deepEqual(clean.map((r) => [r.pos, r.rate]), [["DU", 3], ["CR", 1]]);
});

test("a running source holds a screen wake lock, and stop lets it go (TICK-27)", async () => {
  const p = boot();
  // no wake lock api: nothing to hold
  p.click("sim");
  assert.equal(p.app.state().wake, null);
  p.click("stop");

  const log = fakeWakeLock(p.win);
  p.click("sim");
  await settle();
  assert.equal(log.requests, 1);
  assert.equal(p.app.state().wake, log.sentinels[0]);
  p.click("stop");
  assert.equal(log.released, 1);
  assert.equal(p.app.state().wake, null);

  // the browser drops it when the tab hides; showing the tab takes it again
  p.click("sim");
  await settle();
  log.sentinels[1].lose();
  assert.equal(p.app.state().wake, null);
  Object.defineProperty(p.win.document, "visibilityState", { configurable: true, value: "hidden" });
  p.win.document.dispatchEvent(new p.win.Event("visibilitychange"));
  await settle();
  assert.equal(log.requests, 2, "not while hidden");
  Object.defineProperty(p.win.document, "visibilityState", { configurable: true, value: "visible" });
  p.win.document.dispatchEvent(new p.win.Event("visibilitychange"));
  await settle();
  assert.equal(log.requests, 3);
  assert.equal(p.app.state().wake, log.sentinels[2]);
  // an old sentinel letting go later leaves the new one alone
  log.sentinels[1].lose();
  assert.equal(p.app.state().wake, log.sentinels[2]);
  p.click("stop");

  // a lock granted after stop is handed straight back
  const slow = boot();
  const held = fakeWakeLock(slow.win, { hold: true });
  slow.click("sim");
  slow.click("stop");
  held.resolve[0]();
  await settle();
  assert.equal(held.released, 1);
  assert.equal(slow.app.state().wake, null);

  // a refused lock is shrugged off
  const denied = boot();
  const no = fakeWakeLock(denied.win, { deny: true });
  denied.click("sim");
  await settle();
  assert.equal(no.requests, 1);
  assert.equal(denied.app.state().wake, null);
  assert.equal(denied.app.state().running, true);
});

test("the big readout shows the rate, beat error, status, and trend, and closes (TICK-28)", async () => {
  const p = boot();
  const view = p.$("focus-view");
  assert.equal(view.hidden, true);
  settledSim(p);
  p.click("focus");
  assert.equal(view.hidden, false);
  assert.equal(p.win.document.activeElement, p.$("focus-exit"));
  assert.equal(p.$("focus-rate").textContent, p.$("rate").textContent);
  assert.match(p.$("focus-rate").textContent, /^\+\d+\.\d$/);
  assert.equal(p.$("focus-beat-error").textContent, p.$("beat-error").textContent);
  assert.equal(p.$("focus-status").textContent, "locked");
  // the simulated watch runs steady
  assert.equal(p.$("focus-trend").textContent, "→");
  p.click("focus-exit");
  assert.equal(view.hidden, true);
  assert.equal(p.win.document.activeElement, p.$("focus"));

  // f toggles it, escape closes it
  key(p, "f");
  assert.equal(view.hidden, false);
  key(p, "f");
  assert.equal(view.hidden, true);
  key(p, "f");
  key(p, "Escape");
  assert.equal(view.hidden, true);
  // escape with nothing open does nothing
  key(p, "Escape");
  assert.equal(view.hidden, true);

  // stopped, there is no trend to show
  p.click("stop");
  p.click("focus");
  assert.equal(p.$("focus-trend").textContent, "");
  p.click("focus-exit");

  // where fullscreen works, it goes fullscreen and leaving fullscreen closes it
  const doc = p.win.document;
  let full = null;
  let exits = 0;
  view.requestFullscreen = function () {
    full = this;
    return Promise.resolve();
  };
  Object.defineProperty(doc, "fullscreenElement", { configurable: true, get: () => full });
  doc.exitFullscreen = () => {
    exits++;
    full = null;
  };
  p.click("focus");
  assert.equal(full, view);
  full = null;
  doc.dispatchEvent(new p.win.Event("fullscreenchange"));
  assert.equal(view.hidden, true);
  assert.equal(exits, 0, "already out of fullscreen");
  // closing from the page leaves fullscreen too
  p.click("focus");
  p.click("focus-exit");
  assert.equal(exits, 1);
  // a refused fullscreen still covers the page, and fullscreen changes leave it be
  view.requestFullscreen = () => Promise.reject(new Error("no gesture"));
  p.click("focus");
  await settle();
  doc.dispatchEvent(new p.win.Event("fullscreenchange"));
  assert.equal(view.hidden, false);
  p.click("focus-exit");
  assert.equal(exits, 1);
  // a fullscreen change with the readout closed changes nothing
  doc.dispatchEvent(new p.win.Event("fullscreenchange"));
  assert.equal(view.hidden, true);
});

test("positions save settled results, sum them up, and persist (TICK-29)", async () => {
  const p = boot();
  const add = p.$("session-add");
  assert.equal(add.disabled, true, "nothing to save yet");
  assert.equal(p.$("session-copy").disabled, true);
  assert.equal(p.$("session-clear").disabled, true);
  assert.equal(p.$("session-avg").textContent, "--");
  // a reading still settling can't be saved
  p.click("sim");
  p.advance(5000);
  p.flush();
  assert.equal(p.$("status").textContent, "measuring");
  assert.equal(add.disabled, true);
  p.advance(11000);
  p.flush();
  assert.equal(add.disabled, false);

  const rate = p.$("rate").textContent;
  p.click("session-add");
  const rows = () => [...p.$("session-body").querySelectorAll("tr")].map((tr) => [...tr.children].map((c) => c.textContent));
  assert.deepEqual(rows(), [["dial up", rate, p.$("beat-error").textContent, "28800", "remove"]]);
  assert.equal(p.$("session-note").textContent, "saved dial up");
  // on to the next position not yet saved
  assert.equal(p.$("session-pos").value, "DD");
  p.$("session-pos").value = "CR";
  p.click("session-add");
  assert.equal(p.$("session-pos").value, "DD", "wraps around to the first free one");
  p.$("session-pos").value = "DU";
  p.click("session-add");
  assert.deepEqual(rows().map((r) => r[0]), ["dial up", "crown right"], "a position saved again replaces it");
  assert.equal(p.$("session-delta").textContent, "0.0");
  assert.equal(p.$("session-avg").textContent, rate);

  // the stored table loads back
  const stored = p.win.localStorage.getItem("tick.session");
  const again = boot({ session: stored });
  assert.equal(again.$("session-body").children.length, 2);
  assert.equal(again.$("session-clear").disabled, false);

  // rows follow the target band
  p.change("set-target", "5");
  const cells = p.$("session-body").querySelectorAll("td");
  assert.equal(cells[0].dataset.tolerance, "out");
  p.change("set-target", "10");
  assert.equal(p.$("session-body").querySelectorAll("td")[0].dataset.tolerance, "in");

  // copy puts the text on the clipboard
  let copied = null;
  Object.defineProperty(p.win.navigator, "clipboard", {
    configurable: true,
    value: { writeText: (t) => Promise.resolve((copied = t)) },
  });
  p.click("session-copy");
  await settle();
  assert.equal(copied, tick.sessionText(p.app.state().session));
  assert.equal(p.$("session-note").textContent, "copied");
  Object.defineProperty(p.win.navigator, "clipboard", {
    configurable: true,
    value: { writeText: () => Promise.reject(new Error("blocked")) },
  });
  p.click("session-copy");
  await settle();
  assert.match(p.$("session-note").textContent, /^copy failed/);

  p.$("session-body").querySelector("button").dispatchEvent(new p.win.MouseEvent("click"));
  assert.deepEqual(rows().map((r) => r[0]), ["crown right"]);
  assert.equal(p.$("session-note").textContent, "");
  p.click("session-clear");
  assert.deepEqual(rows(), []);
  assert.equal(p.$("session-delta").textContent, "--");
  assert.equal(p.win.localStorage.getItem("tick.session"), "[]");

  // the button does nothing without a result (a stale click)
  p.click("stop");
  p.click("sim");
  p.click("session-add");
  assert.deepEqual(rows(), []);

  // bad stored json, and storage that throws, start empty
  assert.equal(boot({ session: "{nope" }).$("session-body").children.length, 0);
  const blocked = boot({ storage: "throw" });
  settledSim(blocked);
  blocked.click("session-add");
  assert.equal(blocked.app.state().session.length, 1, "kept for this page");
});

test("an analyzed recording can be saved as a position", async () => {
  const p = boot();
  const data = D.synth({ sampleRate: 48000, bph: 21600, rate: -12, beatError: 1.6 }).next(20 * 48000);
  p.win.OfflineAudioContext = class {
    decodeAudioData() {
      return Promise.resolve({ sampleRate: 48000, length: data.length, numberOfChannels: 1, getChannelData: () => data });
    }
  };
  await p.app.analyzeFile(new p.win.File([new Uint8Array(4)], "w.wav"));
  assert.equal(p.$("status").textContent, "analyzed");
  assert.equal(p.$("beat-error").dataset.level, "warn");
  assert.equal(p.$("session-add").disabled, false);
  p.click("session-add");
  const [row] = p.app.state().session;
  assert.equal(row.bph, 21600);
  assert.ok(Math.abs(row.rate + 12) < 0.5);
  assert.equal(p.$("session-body").querySelectorAll("td")[1].dataset.level, "warn");
});

test("the target band shades the rate graph and marks the readout (TICK-30)", () => {
  const p = boot();
  const bandFill = () => sets(p.ctx("rate-canvas"), "fillStyle").includes("rgba(124, 252, 154, 0.09)");
  assert.equal(bandFill(), false, "off by default");
  p.change("set-target", "30");
  assert.equal(bandFill(), true);
  // the scale grows to hold the band
  assert.ok(texts(p.ctx("rate-canvas")).includes("+60"));
  p.change("set-target", "cosc");
  assert.ok(texts(p.ctx("rate-canvas")).includes("+10"));
  settledSim(p);
  // the simulated watch runs +6.0 s/d, at the edge of -4 to +6
  const rate = Number(p.$("rate").textContent);
  assert.equal(p.$("rate").dataset.tolerance, rate <= 6 ? "in" : "out");
  p.change("set-target", "5");
  assert.equal(p.$("rate").dataset.tolerance, "out");
  p.change("set-target", "off");
  assert.equal(p.$("rate").dataset.tolerance, "");
  assert.equal(p.$("beat-error").dataset.level, "");
  p.click("stop");
  assert.equal(p.$("rate").dataset.tolerance, "", "stopped, the last reading stays up");
});

test("pointing at a graph reads the rate and the beat there (TICK-31)", () => {
  const p = boot();
  const canvas = p.$("rate-canvas");
  assert.ok(canvas.classList.contains("pointable"));
  const point = (id, frac, type = "pointermove", pointerType = "mouse") => {
    const e = new p.win.MouseEvent(type, { clientX: 48 + frac * (900 - 62) });
    Object.defineProperty(e, "pointerType", { value: pointerType });
    p.$(id).dispatchEvent(e);
    p.flush();
  };
  // idle, there is nothing to read
  point("rate-canvas", 0.5);
  assert.ok(!texts(p.ctx("rate-canvas")).some((t) => / at /.test(t)));
  // a full span of readings, so the right edge has one to snap to
  p.change("set-span", "30");
  settledSim(p, 32000);
  point("rate-canvas", 0.4);
  const rateLabel = texts(p.ctx("rate-canvas")).find((t) => / at /.test(t));
  assert.match(rateLabel, /^\+\d+\.\d s\/d at -\d+\.\ds$/);
  assert.match(texts(p.ctx("trace-canvas")).find((t) => / at /.test(t)), /^(tick|tock) [+-]?\d\.\d\d ms at -\d+\.\ds$/);
  // the label sits on whichever side has room
  const align = () => sets(p.ctx("rate-canvas"), "textAlign").at(-1);
  assert.equal(align(), "left");
  point("trace-canvas", 0.95, "pointerdown");
  assert.equal(align(), "right");
  // past the edges it holds to the ends
  point("rate-canvas", -1);
  assert.equal(p.app.state().cursor.frac, 0);
  point("rate-canvas", 2);
  assert.equal(p.app.state().cursor.frac, 1);
  // a mouse leaving clears it; a finger lifting leaves it
  point("rate-canvas", 0.5, "pointerleave", "touch");
  assert.notEqual(p.app.state().cursor, null);
  point("rate-canvas", 0.5, "pointerleave");
  assert.equal(p.app.state().cursor, null);
  assert.ok(!texts(p.ctx("rate-canvas")).some((t) => / at /.test(t)));
  // a new source starts without one
  point("rate-canvas", 0.5);
  p.click("stop");
  p.click("sim");
  assert.equal(p.app.state().cursor, null);
});

test("an analyzed recording reads times from its start under the pointer", async () => {
  const p = boot();
  const data = D.synth({ sampleRate: 48000, bph: 18000, rate: 30 }).next(20 * 48000);
  p.win.OfflineAudioContext = class {
    decodeAudioData() {
      return Promise.resolve({ sampleRate: 48000, length: data.length, numberOfChannels: 1, getChannelData: () => data });
    }
  };
  await p.app.analyzeFile(new p.win.File([new Uint8Array(4)], "w.wav"));
  p.$("rate-canvas").dispatchEvent(new p.win.MouseEvent("pointermove", { clientX: 48 + 0.5 * 838 }));
  p.flush();
  assert.match(texts(p.ctx("rate-canvas")).find((t) => / at /.test(t)), /^\+\d+\.\d s\/d at \d+\.\ds$/);
});

test("a file dropped on the page is analyzed (TICK-32)", async () => {
  const p = boot();
  const data = D.synth({ sampleRate: 48000, bph: 28800 }).next(10 * 48000);
  p.win.OfflineAudioContext = class {
    decodeAudioData() {
      return Promise.resolve({ sampleRate: 48000, length: data.length, numberOfChannels: 1, getChannelData: () => data });
    }
  };
  const doc = p.win.document;
  const file = new p.win.File([new Uint8Array(4)], "dropped.wav");
  const drag = (type, dataTransfer) => {
    const e = new p.win.Event(type, { bubbles: true, cancelable: true });
    Object.defineProperty(e, "dataTransfer", { value: dataTransfer });
    doc.body.dispatchEvent(e);
    return e;
  };
  const files = { types: ["Files"], files: [file] };
  const overlay = p.$("drop");
  assert.equal(overlay.hidden, true);
  assert.ok(drag("dragenter", files).defaultPrevented);
  assert.equal(overlay.hidden, false);
  // moving over a child element nests an enter inside the leave
  drag("dragenter", files);
  drag("dragleave", files);
  assert.equal(overlay.hidden, false);
  assert.ok(drag("dragover", files).defaultPrevented, "the page takes the drop, not the browser");
  drag("dragleave", files);
  assert.equal(overlay.hidden, true);
  drag("dragleave", files);
  assert.equal(overlay.hidden, true, "never counts below zero");

  // dragged text is the browser's business
  const text = drag("dragenter", { types: ["text/plain"], files: [] });
  assert.equal(text.defaultPrevented, false);
  assert.equal(overlay.hidden, true);
  assert.equal(drag("drop", null).defaultPrevented, false);

  drag("dragenter", files);
  drag("drop", files);
  assert.equal(overlay.hidden, true);
  await settle();
  assert.equal(p.$("status").textContent, "analyzed");
  assert.match(p.$("rate-label").textContent, /of dropped\.wav$/);
  // a drop with no file in it
  drag("drop", { types: ["Files"], files: [] });
  assert.equal(overlay.hidden, true);
});

test("settings fold away on a narrow screen and open on a wide one (TICK-33)", () => {
  const wide = boot({ media: () => false });
  assert.equal(wide.$("settings-box").open, true);
  let narrow = true;
  const p = boot({ media: (q) => q === "(max-width: 640px)" && narrow });
  assert.equal(p.$("settings-box").open, false);
  // turning the phone doesn't reopen them; a wide window does
  p.win.dispatchEvent(new p.win.Event("resize"));
  assert.equal(p.$("settings-box").open, false);
  narrow = false;
  p.win.dispatchEvent(new p.win.Event("resize"));
  assert.equal(p.$("settings-box").open, true);
  // no matchMedia: left open
  assert.equal(boot().$("settings-box").open, true);
});

test("keys start and stop the mic and record (TICK-34)", async () => {
  const p = boot();
  const audio = fakeAudio(p.win);
  const e = key(p, " ");
  assert.equal(e, false, "space does not also press a focused button");
  await settle();
  assert.equal(p.app.state().running, true);
  key(p, " ");
  assert.equal(p.app.state().running, false);
  assert.equal(audio.log.closed, 1);

  // not with a modifier, while typing in a setting, or under an open info dialog
  key(p, " ", { ctrlKey: true });
  key(p, "r", { metaKey: true });
  key(p, " ", { altKey: true });
  key(p, " ", {}, p.$("set-correction"));
  key(p, "f", {}, p.$("set-bph"));
  p.win.document.querySelector("[data-info]").dispatchEvent(new p.win.MouseEvent("click"));
  key(p, " ");
  await settle();
  assert.equal(p.app.state().running, false);
  assert.equal(p.$("focus-view").hidden, true);
  key(p, "Escape");
  assert.ok(!p.$("info").hasAttribute("open"));
  // other keys do nothing
  key(p, "x");
  await settle();
  assert.equal(p.app.state().running, false);

  key(p, "r");
  await settle();
  assert.equal(p.app.state().running, true);
  assert.match(p.$("record").textContent, /^recording/);
});

test("recording shows its progress and the save link says what it holds (TICK-35)", async () => {
  const p = boot();
  p.win.URL.createObjectURL = () => "blob:test/1";
  const audio = fakeAudio(p.win);
  const rec = p.$("record");
  assert.equal(rec.dataset.recording, undefined);
  await p.app.record();
  assert.equal(rec.dataset.recording, "");
  assert.equal(rec.style.getPropertyValue("--progress"), "0.0%");
  audio.play({ bph: 18000, amp: 0.05 }, 10);
  p.flush();
  const pct = parseFloat(rec.style.getPropertyValue("--progress"));
  assert.ok(pct > 33 && pct < 34, `${pct}%`);
  audio.play({ bph: 18000, amp: 0.05, seed: 2 }, 21);
  p.flush();
  assert.equal(rec.dataset.recording, undefined);
  assert.equal(rec.style.getPropertyValue("--progress"), "");
  const save = p.$("save");
  assert.equal(save.textContent, "save recording (30 s, 5.8 MB)");
  assert.equal(save.title, `download ${save.download}`);
});

test("only the status is a live region, so readings aren't read out on every beat", () => {
  const p = boot();
  assert.equal(p.$("status").getAttribute("aria-live"), "polite");
  assert.equal(p.$("status").getAttribute("role"), "status");
  assert.equal(p.win.document.querySelectorAll("[aria-live]").length, 1);
  // a frame with no change of state leaves the status text node alone
  p.click("sim");
  const node = p.$("status").firstChild;
  p.advance(200);
  p.flush();
  assert.equal(p.$("status").firstChild, node);
});
