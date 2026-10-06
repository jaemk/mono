// Boots the komino client against jsdom with a fake fetch, websocket,
// location, clipboard, and confirm, so tests drive the real page script.
// The fake server seals reveals with WebCrypto exactly as sealed.rs does.
import { JSDOM } from "jsdom";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { webcrypto } from "node:crypto";

const require = createRequire(import.meta.url);
const html = readFileSync(new URL("../assets/index.html", import.meta.url), "utf8");
export const komino = require("../assets/static/app.js");

const CURVE = { name: "ECDH", namedCurve: "P-256" };
const { b64d, b64e } = komino;

/** A server ECDH key pair, as `GET /komino/api/key` publishes it. */
export async function makeServerKey() {
  const pair = await webcrypto.subtle.generateKey(CURVE, false, ["deriveBits"]);
  const pub = b64e(await webcrypto.subtle.exportKey("raw", pair.publicKey));
  return { pair, pub, kid: pub.slice(0, 16) };
}

/** Seal `plain` (json) to a client's public key, as the server does. */
export async function seal(server, clientPub, aad, plain) {
  const subtle = webcrypto.subtle;
  const client = await subtle.importKey("raw", b64d(clientPub), CURVE, false, []);
  const bits = await subtle.deriveBits({ name: "ECDH", public: client }, server.pair.privateKey, 256);
  const hkdf = await subtle.importKey("raw", bits, "HKDF", false, ["deriveKey"]);
  const salt = webcrypto.getRandomValues(new Uint8Array(16));
  const iv = webcrypto.getRandomValues(new Uint8Array(12));
  const aes = await subtle.deriveKey({ name: "HKDF", hash: "SHA-256", salt, info: new TextEncoder().encode("komino reveal v1") },
    hkdf, { name: "AES-GCM", length: 256 }, false, ["encrypt"]);
  const ct = await subtle.encrypt({ name: "AES-GCM", iv, additionalData: new TextEncoder().encode(aad) },
    aes, new TextEncoder().encode(JSON.stringify(plain)));
  return { kid: server.kid, salt: b64e(salt), iv: b64e(iv), ct: b64e(ct) };
}

const later = (fn, ms) => {
  const t = globalThis.setTimeout(fn, ms);
  if (t && t.unref) t.unref();
  return t;
};

/** A seat; slots are `null` (empty), `"x"` (face down), or a visible value. */
export function seat(player, slots = ["x", "x", "x", "x"], extra = {}) {
  return {
    player,
    slots: slots.map((s) => (s === null ? null : s === "x" ? {} : { v: s })),
    ready: false,
    forfeited: false,
    turns: 1,
    score: null,
    won: false,
    locked: false,
    ...extra,
  };
}

/** A game mid play, p0 (ada) to move, a matchable 4 on the discard. */
export function makeGame(extra = {}) {
  return {
    id: 1,
    version: 3,
    status: "playing",
    seats: [seat("p0"), seat("p1")],
    me: 0,
    turn: 0,
    turn_seq: 5,
    stage: { kind: "start" },
    caller: null,
    final_remaining: [],
    discard_top: 4,
    discard_count: 3,
    discard_seq: 2,
    matchable: true,
    deck_count: 49,
    reveals: [],
    peeked: [],
    can_call: false,
    calling: false,
    ready_deadline: null,
    away_deadline: null,
    score_at: null,
    ...extra,
  };
}

export function member(id, name, extra = {}) {
  return { id, name, removed: false, left: false, present: true, ...extra };
}

export function makeView(game = makeGame(), extra = {}) {
  return {
    room: {
      code: "ABCDEF",
      host: "p0",
      url: "https://kominick.com/komino/r/ABCDEF",
      watch_url: "https://kominick.com/komino/r/ABCDEF/watch",
    },
    me: "p0",
    observer: false,
    observers: 0,
    members: [member("p0", "ada"), member("p1", "bob")],
    game,
    events: [],
    stats: [],
    ...extra,
  };
}

/** An event as views carry them, newest first. */
export function ev(id, kind, player = null, payload = {}) {
  return { id, kind, player, payload };
}

/**
 * A Web Audio stand-in. `played` lists every sound source started, as
 * `{ kind: "noise" | <oscillator type>, at, f }`.
 */
export function fakeAudio() {
  const contexts = [];
  const param = () => {
    const p = { value: 0, setValueAtTime: (v) => (p.value = v), exponentialRampToValueAtTime: (v) => (p.to = v) };
    return p;
  };
  class Ctx {
    constructor() {
      contexts.push(this);
      this.state = "suspended";
      this.sampleRate = 8000;
      this.currentTime = 10;
      this.destination = {};
      this.played = [];
      this.resumes = 0;
    }
    resume() {
      this.resumes++;
      this.state = "running";
      return Promise.resolve();
    }
    node(fields, start) {
      return Object.assign({ connect: (next) => next, start, stop() {} }, fields);
    }
    createBuffer(_channels, len) {
      const data = new Float32Array(len);
      return { getChannelData: () => data };
    }
    createBufferSource() {
      return this.node({}, (at) => this.played.push({ kind: "noise", at }));
    }
    createBiquadFilter() {
      return this.node({ Q: param(), frequency: param() });
    }
    createGain() {
      return this.node({ gain: param() });
    }
    createOscillator() {
      const osc = this.node({ frequency: param() }, (at) => this.played.push({ kind: osc.type, at, f: osc.frequency.value }));
      return osc;
    }
  }
  return {
    Ctx,
    contexts,
    get played() {
      return contexts.flatMap((c) => c.played);
    },
    clear() {
      for (const c of contexts) c.played = [];
    },
  };
}

/** Let pending promise callbacks run. */
export async function flush() {
  for (let i = 0; i < 5; i++) await new Promise((r) => setImmediate(r));
}

/** Spin the event loop until `ready()` holds; WebCrypto finishes off-thread. */
export async function settle(ready) {
  for (let i = 0; i < 2000; i++) {
    if (ready()) return;
    await new Promise((r) => setImmediate(r));
  }
  throw new Error("timed out waiting for the page to settle");
}

/**
 * Start the page at `path`. `routes` maps `"METHOD /url"` to `[status, body]`
 * or a function returning one; unrouted requests get a 404.
 */
export async function boot({
  path = "/komino/r/ABCDEF",
  me = { id: "p0", name: "ada" },
  view = makeView(),
  routes = {},
  secrets = {},
  confirmAnswer = true,
  clipboardFails = false,
  subtle = webcrypto.subtle,
  // a Web Audio constructor, e.g. FakeAudio below; none means no audio
  AudioContext,
  // replaces the page's localStorage, e.g. one that throws
  storage,
} = {}) {
  const dom = new JSDOM(html, { url: "https://kominick.com" + path });
  const w = dom.window;
  const calls = [];
  const server = { key: await makeServerKey() };
  const table = {
    "GET /komino/api/me": [200, me],
    "POST /komino/api/rooms/ABCDEF/join": [200, view],
    "GET /komino/api/rooms/ABCDEF/watch": [200, view],
    "GET /komino/api/key": () => [200, { kid: server.key.kid, public_key: server.key.pub }],
    // a reveal answers from `secrets` by "opening", "drawn", or "peek:<id>",
    // sealed to the client key like the real server; a [status, body] pair
    // in `secrets` is returned as is
    "POST /komino/api/rooms/ABCDEF/reveal": async (init) => {
      const req = JSON.parse(init.body);
      const name = req.what === "peek" ? `peek:${req.id}` : req.what;
      let secret = secrets[name];
      if (typeof secret === "function") secret = secret(req);
      if (secret === undefined) return [403, { code: "forbidden", message: "that card is not yours to see now" }];
      if (Array.isArray(secret)) return secret;
      return [200, await seal(server.key, req.client_key, "ABCDEF", secret)];
    },
    ...routes,
  };
  const fetch = async (url, init) => {
    const key = `${init.method} ${url}`;
    calls.push({ key, body: init.body ? JSON.parse(init.body) : undefined });
    const route = table[key];
    const [status, body] = typeof route === "function" ? await route(init) : route || [404, { code: "not_found", message: "no such room" }];
    return { ok: status < 400, status, json: async () => body };
  };

  const sockets = [];
  class FakeSocket {
    constructor(url) {
      this.url = url;
      this.readyState = 0;
      this.sent = [];
      this.closed = false;
      sockets.push(this);
    }
    send(text) {
      this.sent.push(JSON.parse(text));
    }
    close() {
      this.closed = true;
      this.readyState = 3;
    }
    // test side
    open() {
      this.readyState = FakeSocket.OPEN;
      if (this.onopen) this.onopen();
    }
    receive(msg) {
      this.onmessage({ data: JSON.stringify(msg) });
    }
    drop() {
      this.readyState = 3;
      if (this.onclose) this.onclose();
    }
  }
  FakeSocket.OPEN = 1;

  const location = { pathname: path, protocol: "https:", host: "kominick.com", href: "https://kominick.com" + path };
  // the page's monotonic clock, moved by hand
  const clock = { ms: 1000 };
  const confirms = [];
  const clipboard = [];
  const env = {
    document: w.document,
    crypto: { subtle },
    location,
    fetch,
    WebSocket: FakeSocket,
    setTimeout: later,
    clearTimeout: (t) => globalThis.clearTimeout(t),
    setInterval: (fn, ms) => {
      const t = globalThis.setInterval(fn, ms);
      if (t && t.unref) t.unref();
      return t;
    },
    clearInterval: (t) => globalThis.clearInterval(t),
    performance: { now: () => clock.ms },
    AudioContext,
    localStorage: storage === undefined ? w.localStorage : storage,
    confirm: (msg) => {
      confirms.push(msg);
      return confirmAnswer;
    },
    navigator: {
      clipboard: {
        writeText: async (text) => {
          if (clipboardFails) throw new Error("denied");
          clipboard.push(text);
        },
      },
    },
  };

  const app = komino.createKomino(env);
  await app.start();
  await flush();
  const $ = (id) => w.document.getElementById(id);
  const page = {
    app,
    dom,
    w,
    doc: w.document,
    $,
    calls,
    sockets,
    location,
    confirms,
    clipboard,
    /** Move the page's monotonic clock forward. */
    tick(ms) {
      clock.ms += ms;
    },
    get socket() {
      return sockets[sockets.length - 1];
    },
    /** The open socket, opened if it isn't yet. */
    live() {
      const s = sockets[sockets.length - 1];
      if (s.readyState !== FakeSocket.OPEN) s.open();
      return s;
    },
    push(v) {
      page.live().receive({ type: "view", view: v });
    },
    slot(seatIdx, slotIdx) {
      return w.document.querySelector(`button[data-seat="${seatIdx}"][data-slot="${slotIdx}"]`);
    },
    controls() {
      return [...$("controls").querySelectorAll("button")].map((b) => b.textContent);
    },
    control(label) {
      return [...$("controls").querySelectorAll("button")].find((b) => b.textContent === label);
    },
    confirmOpen() {
      return !$("confirm").hidden;
    },
    confirmText() {
      return $("confirm-text").textContent;
    },
    ok() {
      $("confirm-ok").click();
    },
    toast() {
      return $("toast").hidden ? null : $("toast").textContent;
    },
    /** Calls to `METHOD url`, with their json bodies. */
    callsTo(key) {
      return calls.filter((c) => c.key === key);
    },
    reveals() {
      return calls.filter((c) => c.key === "POST /komino/api/rooms/ABCDEF/reveal").map((c) => c.body);
    },
    /** Replace the server's key pair, as a key rotation would. */
    async rotateServerKey() {
      server.key = await makeServerKey();
    },
    settle,
    /** Let in-flight fetches and crypto finish when nothing visible changes. */
    async idle() {
      for (let i = 0; i < 300; i++) await new Promise((r) => setImmediate(r));
    },
    label(seatIdx, slotIdx) {
      return page.slot(seatIdx, slotIdx).getAttribute("aria-label");
    },
    stop() {
      app.stop();
      w.close();
    },
  };
  return page;
}
