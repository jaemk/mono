// Boots the tick page against jsdom with a recording 2d context, controllable
// timers and animation frames, and fake microphone and audio classes, so tests
// drive the real page script.
import { JSDOM } from "jsdom";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const html = readFileSync(new URL("../assets/index.html", import.meta.url), "utf8");
export const tick = require("../assets/static/app.js");
export const D = require("../assets/static/detector.js");

/** A 2d context that records every call and property write. */
export function fakeCtx() {
  const state = { calls: [] };
  return new Proxy(state, {
    get(t, k) {
      if (k in t) return t[k];
      return (...args) => {
        t.calls.push([k, ...args]);
      };
    },
    set(t, k, v) {
      t[k] = v;
      t.calls.push(["set", k, v]);
      return true;
    },
  });
}

/** The calls of the latest frame: each draw starts with setTransform. */
export function frameCalls(ctx) {
  let start = 0;
  ctx.calls.forEach((c, i) => {
    if (c[0] === "setTransform") start = i;
  });
  return ctx.calls.slice(start);
}

/** Texts drawn with fillText in the latest frame. */
export const texts = (ctx) => frameCalls(ctx).filter((c) => c[0] === "fillText").map((c) => c[1]);
export const callsOf = (ctx, name) => frameCalls(ctx).filter((c) => c[0] === name);
export const sets = (ctx, prop) => frameCalls(ctx).filter((c) => c[0] === "set" && c[1] === prop).map((c) => c[2]);

/**
 * Boot the page. `storage` seeds localStorage (a string, or "throw" for a
 * storage that throws), `canvas: false` makes getContext return null, and
 * `raf: false` removes requestAnimationFrame, `media` answers matchMedia
 * queries (a function of the query), and `session` seeds the saved positions.
 */
export function boot({ storage, canvas = true, raf = true, dpr, media, session } = {}) {
  const dom = new JSDOM(html, { url: "https://example.test/tick" });
  const win = dom.window;
  if (media) win.matchMedia = (q) => ({ matches: media(q) });
  if (session !== undefined) win.localStorage.setItem("tick.session", session);
  const ctxs = new Map();
  win.HTMLCanvasElement.prototype.getContext = function () {
    if (!canvas) return null;
    if (!ctxs.has(this.id)) ctxs.set(this.id, fakeCtx());
    return ctxs.get(this.id);
  };
  if (dpr) win.devicePixelRatio = dpr;

  if (storage === "throw") {
    Object.defineProperty(win, "localStorage", {
      configurable: true,
      get() {
        throw new Error("blocked");
      },
    });
  } else if (storage !== undefined) {
    win.localStorage.setItem("tick.settings", storage);
  }

  // animation frames and timers run only when a test flushes them
  const frames = [];
  if (raf) win.requestAnimationFrame = (fn) => frames.push(fn);
  else win.requestAnimationFrame = undefined;
  const timeouts = [];
  win.setTimeout = (fn) => timeouts.push(fn);
  const intervals = new Map();
  let nextId = 1;
  win.setInterval = (fn, ms) => {
    intervals.set(nextId, { fn, ms });
    return nextId++;
  };
  win.clearInterval = (id) => intervals.delete(id);
  let clock = 0;
  Object.defineProperty(win, "performance", { configurable: true, value: { now: () => clock } });

  const app = tick.createTick(win);
  app.start();
  const $ = (id) => win.document.getElementById(id);

  return {
    win,
    app,
    $,
    ctxs,
    ctx: (id) => ctxs.get(id),
    intervals,
    /** Run pending animation frames (and timeout-scheduled renders). */
    flush() {
      while (frames.length) frames.shift()();
      while (timeouts.length) timeouts.shift()();
    },
    /** Advance the fake clock, firing every interval each `step` ms. */
    advance(ms, step = 50) {
      for (let t = 0; t < ms; t += step) {
        clock += step;
        for (const { fn } of [...intervals.values()]) fn();
      }
    },
    change(id, value) {
      const node = $(id);
      node.value = value;
      node.dispatchEvent(new win.Event("change"));
    },
    click(id) {
      $(id).dispatchEvent(new win.MouseEvent("click"));
    },
  };
}

/** Wait for queued promise callbacks to run. */
export const settle = () => new Promise((r) => setImmediate(r));

/**
 * Fake microphone and audio graph. `worklet: false` leaves out
 * AudioWorklet so the page falls back to a ScriptProcessor. Returns hooks to
 * push audio and to observe cleanup.
 */
export function fakeAudio(win, { worklet = true, deny, addModule, gum } = {}) {
  const log = { tracksStopped: 0, closed: 0, connected: [], modules: [], constraints: null };
  const track = { stop: () => log.tracksStopped++ };
  const stream = { getTracks: () => [track] };
  Object.defineProperty(win.navigator, "mediaDevices", {
    configurable: true,
    value: {
      getUserMedia: (c) => {
        log.constraints = c;
        if (gum) return gum();
        if (deny) return Promise.reject(deny);
        return Promise.resolve(stream);
      },
    },
  });
  let node = null;
  const connectable = (name) => ({ name, connect: (to) => log.connected.push([name, to && to.name]) });
  class FakeContext {
    constructor() {
      this.sampleRate = 48000;
      this.destination = { name: "destination" };
      if (worklet) {
        this.audioWorklet = {
          addModule: (url) => {
            log.modules.push(url);
            return addModule ? addModule() : Promise.resolve();
          },
        };
      }
    }
    createMediaStreamSource() {
      return connectable("input");
    }
    createGain() {
      return { ...connectable("mute"), gain: { value: 1 } };
    }
    createScriptProcessor(size) {
      log.blockSize = size;
      node = connectable("processor");
      return node;
    }
    resume() {
      return Promise.resolve();
    }
    close() {
      log.closed++;
    }
  }
  win.AudioContext = FakeContext;
  if (worklet) {
    win.AudioWorkletNode = class {
      constructor(ctx, name) {
        log.processor = name;
        this.name = "worklet";
        this.port = {};
        this.connect = (to) => log.connected.push(["worklet", to && to.name]);
        node = this;
      }
    };
  } else {
    delete win.AudioWorkletNode;
  }
  return {
    log,
    /** Send `secs` of a synthesized watch through the current audio node. */
    play(watch, secs) {
      const gen = D.synth({ sampleRate: 48000, ...watch });
      for (let i = 0; i < secs * 48000; i += 2048) {
        const block = gen.next(2048);
        if (worklet) node.port.onmessage({ data: block });
        else node.onaudioprocess({ inputBuffer: { getChannelData: () => block } });
      }
    },
  };
}
