// Boots the komino client against jsdom with a fake fetch, websocket,
// location, clipboard, and confirm, so tests drive the real page script.
import { JSDOM } from "jsdom";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const html = readFileSync(new URL("../assets/index.html", import.meta.url), "utf8");
export const komino = require("../assets/static/app.js");

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

/** Let pending promise callbacks run. */
export async function flush() {
  for (let i = 0; i < 5; i++) await new Promise((r) => setImmediate(r));
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
  confirmAnswer = true,
  clipboardFails = false,
} = {}) {
  const dom = new JSDOM(html, { url: "https://kominick.com" + path });
  const w = dom.window;
  const calls = [];
  const table = {
    "GET /komino/api/me": [200, me],
    "POST /komino/api/rooms/ABCDEF/join": [200, view],
    "GET /komino/api/rooms/ABCDEF/watch": [200, view],
    ...routes,
  };
  const fetch = async (url, init) => {
    const key = `${init.method} ${url}`;
    calls.push({ key, body: init.body ? JSON.parse(init.body) : undefined });
    const route = table[key];
    const [status, body] = typeof route === "function" ? route(init) : route || [404, { code: "not_found", message: "no such room" }];
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
  const confirms = [];
  const clipboard = [];
  const env = {
    document: w.document,
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
    stop() {
      app.stop();
      w.close();
    },
  };
  return page;
}
