// Post-deploy acceptance checks against the live site. Every mounted app gets
// at least one request that reaches its handlers, and the apps with storage
// get a write and a read back. Writes are kept short-lived: pastes expire in
// a minute and komino rooms are left at the end (idle ones are swept).
//
//   make acceptance                 # against production
//   ACCEPTANCE_BASE=http://localhost:3000 ACCEPTANCE_HOSTS=0 make acceptance
//
// ACCEPTANCE_VERSION, when set, must match the 7-char commit /status reports.
// ACCEPTANCE_HOSTS=0 skips the checks that route on a real hostname.
//
// Needs node 22+ (global fetch, WebSocket with headers, and WebCrypto).

import { test } from "node:test";
import assert from "node:assert/strict";

const BASE = (process.env.ACCEPTANCE_BASE || "https://kominick.com").replace(/\/$/, "");
const HOSTS = process.env.ACCEPTANCE_HOSTS !== "0";
const VERSION = process.env.ACCEPTANCE_VERSION || "";
const TIMEOUT_MS = 15000;

// ---------------------------------------------------------------------------
// http with a cookie jar per client
// ---------------------------------------------------------------------------

class Client {
  constructor(base = BASE) {
    this.base = base;
    this.cookies = new Map();
  }

  cookieHeader() {
    return [...this.cookies].map(([k, v]) => `${k}=${v}`).join("; ");
  }

  async req(method, path, { json, body, headers = {}, redirect = "manual" } = {}) {
    const h = { ...headers };
    if (this.cookies.size) h.cookie = this.cookieHeader();
    if (json !== undefined) {
      h["content-type"] = "application/json";
      body = JSON.stringify(json);
    }
    const resp = await fetch(this.base + path, {
      method,
      headers: h,
      body,
      redirect,
      signal: AbortSignal.timeout(TIMEOUT_MS),
    });
    for (const c of resp.headers.getSetCookie()) {
      const [pair] = c.split(";");
      const at = pair.indexOf("=");
      this.cookies.set(pair.slice(0, at).trim(), pair.slice(at + 1).trim());
    }
    return resp;
  }

  get(path, opts) {
    return this.req("GET", path, opts);
  }

  post(path, opts) {
    return this.req("POST", path, opts);
  }

  async json(method, path, opts, status = 200) {
    const resp = await this.req(method, path, opts);
    const text = await resp.text();
    assert.equal(resp.status, status, `${method} ${path}: ${resp.status} ${text}`);
    return JSON.parse(text);
  }
}

async function expectStatus(resp, status, what) {
  const text = await resp.text();
  assert.equal(resp.status, status, `${what}: ${resp.status} ${text.slice(0, 300)}`);
  return text;
}

function expectRedirect(resp, location, what) {
  assert.ok([301, 302, 303, 307, 308].includes(resp.status), `${what}: got ${resp.status}`);
  const got = resp.headers.get("location");
  if (location instanceof RegExp) assert.match(got, location, what);
  else assert.equal(got, location, what);
}

// ---------------------------------------------------------------------------
// mono: status, homepage hosts, host-routed pages
// ---------------------------------------------------------------------------

test("mono status reports ok and the deployed commit", async () => {
  const body = await new Client().json("GET", "/status");
  assert.equal(body.ok, "ok");
  assert.match(body.version, /^[0-9a-f]{7}$/);
  if (VERSION) assert.equal(body.version, VERSION, "the live commit is not the expected one");
});

test("homepage, robots, favicon, and the json 404", async () => {
  const c = new Client();
  const home = await expectStatus(await c.get("/"), 200, "homepage");
  assert.match(home, /<html/i);
  await expectStatus(await c.get("/robots.txt"), 200, "robots.txt");
  const icon = await c.get("/favicon.ico");
  await expectStatus(icon, 200, "favicon");
  const missing = await c.get("/definitely/not/here");
  const text = await expectStatus(missing, 404, "unknown path");
  assert.deepEqual(JSON.parse(text), { code: 404, message: "NOT_FOUND" });
});

const hostTest = HOSTS ? test : test.skip;

hostTest("every homepage host serves the homepage", async () => {
  for (const host of ["kominick.com", "james.kominick.com", "kominick.org"]) {
    const text = await expectStatus(await new Client(`https://${host}`).get("/"), 200, host);
    assert.match(text, /<html/i, host);
  }
  // the dev host answers with the version
  const dev = await new Client("https://jaemk.me").json("GET", "/");
  assert.equal(dev.ok, "ok");
});

hostTest("ip, ugh, and outside are routed by host", async () => {
  const ip = await expectStatus(await new Client("https://ip.kominick.com").get("/"), 200, "ip");
  assert.match(ip.trim(), /^[0-9a-f.:]+$/i, "ip page should be just the caller's address");
  const ugh = await expectStatus(await new Client("https://ugh.kominick.com").get("/"), 200, "ugh");
  assert.match(ugh, /business days left in year: \d+/);
  const outside = await new Client("https://outside.kominick.com").get("/");
  await expectStatus(outside, 200, "outside");
});

hostTest("git.jaemk.me redirects to github", async () => {
  const c = new Client("https://git.jaemk.me");
  expectRedirect(await c.get("/"), "https://github.com/jaemk/", "git root");
  expectRedirect(await c.get("/mono"), "https://github.com/jaemk/mono", "git repo");
});

// ---------------------------------------------------------------------------
// spot
// ---------------------------------------------------------------------------

hostTest("spotie.app lands on spot", async () => {
  expectRedirect(await new Client("https://spotie.app").get("/"), "/spot", "spotie root");
});

test("spot is up and sends strangers to log in", async () => {
  const c = new Client();
  const status = await c.json("GET", "/spot/status");
  assert.equal(status.ok, "ok");
  for (const path of ["/spot", "/spot/api/current", "/spot/api/top"]) {
    const resp = await c.get(path);
    assert.ok(resp.status >= 300 && resp.status < 400, `${path}: got ${resp.status}`);
    await resp.text();
  }
  // login writes a one-time token to the db and hands off to spotify
  expectRedirect(await c.get("/spot/login"), /^https:\/\/accounts\.spotify\.com\//, "spot login");
});

// ---------------------------------------------------------------------------
// paste (db + s3)
// ---------------------------------------------------------------------------

hostTest("paste.kominick.com lands on paste", async () => {
  expectRedirect(await new Client("https://paste.kominick.com").get("/"), "/paste", "paste root");
});

test("paste stores, serves, and encrypts", async () => {
  const c = new Client();
  const status = await c.json("GET", "/paste/status");
  assert.match(status.hash, /^[0-9a-f]{7}$/);
  if (VERSION) assert.equal(status.hash, VERSION);
  const editor = await expectStatus(await c.get("/paste"), 200, "paste editor");
  assert.match(editor, /<html/i);

  const content = `acceptance ${new Date().toISOString()} ${crypto.randomUUID()}`;
  const made = await c.json("POST", "/paste/new?type=text&ttl_seconds=60", { body: content });
  assert.equal(made.message, "success");
  const raw = await expectStatus(await c.get(`/paste/raw/${made.key}`), 200, "raw paste");
  assert.equal(raw, content);
  const viewed = await c.json("GET", `/paste/json/${made.key}`);
  assert.equal(viewed.paste.content, content);
  const page = await expectStatus(await c.get(`/paste/${made.key}`), 200, "paste page");
  assert.ok(page.includes(made.key));

  const key = crypto.randomUUID();
  const secret = await c.json("POST", "/paste/new?type=text&ttl_seconds=60", {
    body: content,
    headers: { "x-paste-encryption-key": key },
  });
  const locked = await c.get(`/paste/raw/${secret.key}`);
  const err = JSON.parse(await expectStatus(locked, 400, "encrypted paste without a key"));
  assert.equal(err.error, "decryption_key_required");
  const opened = await c.get(`/paste/raw/${secret.key}`, {
    headers: { "x-paste-encryption-key": key },
  });
  assert.equal(await expectStatus(opened, 200, "encrypted paste with its key"), content);

  await expectStatus(await c.get("/paste/json/doesnotexist0"), 404, "missing paste");
});

// ---------------------------------------------------------------------------
// flip
// ---------------------------------------------------------------------------

test("flip serves the page, flips, and keeps odds and coin per cookie", async () => {
  const c = new Client();
  for (const path of ["/flip", "/flip/odds"]) {
    assert.match(await expectStatus(await c.get(path), 200, path), /<html/i);
  }
  assert.deepEqual(await c.json("GET", "/flip/api/odds"), { heads_pct: 50, step: 10 });
  const fair = await c.json("POST", "/flip/api/flip");
  assert.ok(["heads", "tails"].includes(fair.result));

  await c.json("POST", "/flip/api/odds", { json: { heads_pct: 100 } });
  assert.equal(c.cookies.has("flip_odds"), true);
  for (let i = 0; i < 5; i++) {
    assert.equal((await c.json("POST", "/flip/api/flip")).result, "heads");
  }
  await c.json("POST", "/flip/api/odds", { json: { heads_pct: 55 } }, 400);

  const coin = await c.json("POST", "/flip/api/coin", {
    json: { heads_name: "yes", tails_name: "no", heads_color: "#FFFFFF", tails_color: "#000000" },
  });
  assert.equal(coin.heads.name, "yes");
  assert.equal(coin.heads.color, "#ffffff");
  assert.equal(coin.tails.ink, "#f4f2ef");
  const page = await expectStatus(await c.get("/flip"), 200, "customized flip");
  assert.ok(page.includes("yes"), "the coin cookie should render into the page");
  await c.json(
    "POST",
    "/flip/api/coin",
    { json: { heads_name: "", tails_name: "no", heads_color: "#ffffff", tails_color: "red" } },
    400,
  );
});

// ---------------------------------------------------------------------------
// mapour (mounted only when MAPOUR_ENABLED=true)
// ---------------------------------------------------------------------------

test("mapour answers when enabled, and is not mounted otherwise", async () => {
  const c = new Client();
  const status = await c.get("/mapour/status");
  if (status.status === 404) {
    await status.text();
    // disabled: nothing under /mapour reaches a handler
    await expectStatus(await c.get("/mapour/api/me"), 404, "disabled mapour");
    return;
  }
  await expectStatus(status, 200, "mapour status");
  const me = await c.get("/mapour/api/me");
  assert.equal(me.status, 401, `anonymous mapour me: ${me.status}`);
  await me.text();
});

// ---------------------------------------------------------------------------
// komino: rooms, sealed reveals, observers
// ---------------------------------------------------------------------------

const b64d = (s) => Buffer.from(s, "base64url");
const b64e = (b) => Buffer.from(b).toString("base64url");
const subtle = globalThis.crypto.subtle;
const P256 = { name: "ECDH", namedCurve: "P-256" };

/** A client key pair, as the browser makes one (SEAL-5). */
async function clientKey() {
  const pair = await subtle.generateKey(P256, false, ["deriveBits"]);
  const pub = b64e(await subtle.exportKey("raw", pair.publicKey));
  return { pair, pub };
}

async function openSealed(key, serverPublic, code, sealed) {
  const server = await subtle.importKey("raw", b64d(serverPublic), P256, false, []);
  const shared = await subtle.deriveBits({ name: "ECDH", public: server }, key.pair.privateKey, 256);
  const hkdf = await subtle.importKey("raw", shared, "HKDF", false, ["deriveKey"]);
  const aes = await subtle.deriveKey(
    { name: "HKDF", hash: "SHA-256", salt: b64d(sealed.salt), info: Buffer.from("komino reveal v1") },
    hkdf,
    { name: "AES-GCM", length: 256 },
    false,
    ["decrypt"],
  );
  const plain = await subtle.decrypt(
    { name: "AES-GCM", iv: b64d(sealed.iv), additionalData: Buffer.from(code) },
    aes,
    b64d(sealed.ct),
  );
  return JSON.parse(Buffer.from(plain).toString());
}

async function reveal(c, code, key, what, id, status = 200) {
  const resp = await c.post(`/komino/api/rooms/${code}/reveal`, {
    json: { client_key: key.pub, what, id },
  });
  const text = await resp.text();
  assert.equal(resp.status, status, `reveal ${what}: ${resp.status} ${text}`);
  if (status === 200) {
    assert.equal(resp.headers.get("cache-control"), "no-store");
    assert.ok(!text.includes('"v"'), `a reveal travelled unsealed: ${text}`);
  }
  return JSON.parse(text);
}

/** Wait for the first message on a socket matching `f`. */
function nextMessage(ws, f) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("no matching socket message")), TIMEOUT_MS);
    ws.addEventListener("message", function on(ev) {
      const msg = JSON.parse(ev.data);
      if (f(msg)) {
        clearTimeout(timer);
        ws.removeEventListener("message", on);
        resolve(msg);
      }
    });
    ws.addEventListener("error", () => reject(new Error("socket error")), { once: true });
  });
}

function socket(c, path) {
  const url = c.base.replace(/^http/, "ws") + path;
  return new WebSocket(url, { headers: { cookie: c.cookieHeader() } });
}

test("komino plays a sealed round with an observer watching", async (t) => {
  const host = new Client();
  const guest = new Client();
  const watcher = new Client();
  const page = await expectStatus(await host.get("/komino"), 200, "komino page");
  assert.match(page, /<html/i);
  // the deployed page carries the sound toggle and the effect layers
  for (const id of ["sound", "caption", "fx"]) assert.match(page, new RegExp(`id="${id}"`), `komino page #${id}`);
  assert.ok(host.cookies.has("komino_player"), "the page should issue a player cookie");
  await expectStatus(await guest.get("/komino"), 200, "komino page");
  const script = await expectStatus(await host.get("/komino/static/app.js"), 200, "komino app.js");
  // other hands turned toward their owners, slot numbers on every card
  assert.match(script, /grid-area/, "komino app.js turns other players' hands");
  assert.match(script, /slot-no/, "komino app.js numbers the cards");
  assert.match(script, /reaction_ms/, "komino app.js reports match reaction times");
  assert.match(script, /show_misses/, "komino app.js sends the missed match setting");
  assert.match(page, /id="set-misses"/, "komino page offers the missed match setting");

  const server = await host.json("GET", "/komino/api/key");
  assert.match(server.kid, /^[0-9a-f]{16}$/);
  assert.equal(b64d(server.public_key).length, 65);

  const hostMe = await host.json("POST", "/komino/api/me", { json: { name: "accept-host" } });
  await guest.json("POST", "/komino/api/me", { json: { name: "accept-guest" } });
  const room = await host.json("POST", "/komino/api/rooms");
  const code = room.room.code;
  assert.match(code, /^[A-Z0-9]{6}$/);
  // no body takes every default setting
  assert.deepEqual(room.room.settings, {
    hand_size: 4, away_grace_secs: 30, turn_limit_secs: null, reveal_secs: 15, show_misses: true,
  });
  t.after(async () => {
    // leaving as host ends the game; the empty room is swept later
    await guest.post(`/komino/api/rooms/${code}/leave`).then((r) => r.text());
    await host.post(`/komino/api/rooms/${code}/leave`).then((r) => r.text());
  });
  await guest.json("POST", `/komino/api/rooms/${code}/join`);
  let view = await host.json("POST", `/komino/api/rooms/${code}/action`, { json: { type: "start" } });
  assert.equal(view.game.status, "peeking");
  assert.ok(!JSON.stringify(view.game).includes('"v"'), "a view carried a card value");
  assert.equal(typeof view.server_now, "number");

  // each player opens their own two cards, sealed to a fresh key
  const keys = {};
  const known = {};
  for (const [name, c] of [["host", host], ["guest", guest]]) {
    keys[name] = await clientKey();
    const sealed = await reveal(c, code, keys[name], "opening");
    assert.equal(sealed.kid, server.kid);
    const opened = await openSealed(keys[name], server.public_key, code, sealed);
    assert.equal(opened.cards.length, 2);
    for (const card of opened.cards) assert.equal(typeof card.v, "number");
    known[name] = opened.cards;
  }
  // a key belongs to one player, and nobody else's room is readable
  await reveal(guest, code, keys.host, "opening", undefined, 403);
  await reveal(host, code, keys.host, "peek", undefined, 400);
  await host.json("POST", `/komino/api/rooms/${code}/reveal`, {
    json: { client_key: "not a key", what: "opening" },
  }, 400);

  // an observer sees the table but no values, and cannot act or reveal
  const watched = await watcher.json("GET", `/komino/api/rooms/${code}/watch`);
  assert.equal(watched.observer, true);
  assert.equal(watched.me, null);
  assert.equal(watched.members.length, 2);
  assert.ok(watched.room.watch_url.endsWith(`/komino/r/${code}/watch`));
  assert.ok(!JSON.stringify(watched.game).includes('"v"'), "an observer saw a card value");
  await expectStatus(await watcher.get(`/komino/r/${code}/watch`), 200, "watch page");
  await watcher.json("GET", `/komino/api/rooms/${code}`, {}, 403);
  await watcher.json("POST", `/komino/api/rooms/${code}/action`, { json: { type: "ready" } }, 403);
  await reveal(watcher, code, await clientKey(), "opening", undefined, 403);

  const ws = socket(watcher, `/komino/r/${code}/watch/ws`);
  t.after(() => ws.close());
  const first = await nextMessage(ws, (m) => m.type === "view");
  assert.equal(first.view.observer, true);
  ws.send(JSON.stringify({ type: "ready", ref: 1 }));
  const refused = await nextMessage(ws, (m) => m.type === "result");
  assert.equal(refused.ok, false);
  view = await host.json("GET", `/komino/api/rooms/${code}`);
  assert.equal(view.observers, 1, "the watch socket should hold an observer slot");

  // both ready up, and whoever's turn it is draws and reveals the card
  await host.json("POST", `/komino/api/rooms/${code}/action`, { json: { type: "ready" } });
  view = await guest.json("POST", `/komino/api/rooms/${code}/action`, { json: { type: "ready" } });
  assert.equal(view.game.status, "playing");
  const turnPlayer = view.game.seats[view.game.turn].player;
  const [mover, moverKey] = turnPlayer === hostMe.id ? [host, keys.host] : [guest, keys.guest];
  view = await mover.json("POST", `/komino/api/rooms/${code}/action`, {
    json: { type: "draw", turn_seq: view.game.turn_seq },
  });
  assert.equal(view.game.stage.card ?? null, null, "the drawn card travelled in the view");
  const drawn = await openSealed(
    moverKey,
    server.public_key,
    code,
    await reveal(mover, code, moverKey, "drawn"),
  );
  assert.equal(typeof drawn.card, "number");
  view = await mover.json("POST", `/komino/api/rooms/${code}/action`, {
    json: { type: "swap", slot: 0, turn_seq: view.game.turn_seq },
  });
  assert.notEqual(view.game.seats[view.game.turn].player, turnPlayer, "the turn should pass");

  // the host matches its own slot 2 against the swapped-out discard; a miss
  // shows the card's value to everyone in a room that shows misses
  const hostSeat = view.game.seats.findIndex((s) => s.player === hostMe.id);
  const mine = known.host.find((c) => c.slot === 1).v;
  const top = view.game.discard_top;
  view = await host.json("POST", `/komino/api/rooms/${code}/action`, {
    json: { type: "match", seq: view.game.discard_seq, seat: hostSeat, slot: 1 },
  });
  const matched = view.events.find((e) => e.kind === "match");
  assert.equal(matched.payload.ok, mine === top);
  if (mine !== top) {
    assert.equal(matched.payload.value, mine, "a shown miss names the card");
    const seen = await guest.json("GET", `/komino/api/rooms/${code}`);
    assert.equal(seen.events.find((e) => e.kind === "match").payload.value, mine);
  }
});

test("komino rooms play by the settings they were created with", async (t) => {
  const host = new Client();
  const guest = new Client();
  await expectStatus(await host.get("/komino"), 200, "komino page");
  await expectStatus(await guest.get("/komino"), 200, "komino page");
  const server = await host.json("GET", "/komino/api/key");

  // out of range settings create nothing
  for (const bad of [{ hand_size: 11 }, { hand_size: 3 }, { turn_limit_secs: 5 }, { reveal_secs: 61 }, { show_misses: null }]) {
    const err = await host.json("POST", "/komino/api/rooms", { json: bad }, 400);
    assert.equal(err.code, "invalid", JSON.stringify(bad));
  }

  const settings = { hand_size: 6, away_grace_secs: 15, turn_limit_secs: 60, reveal_secs: null, show_misses: false };
  const room = await host.json("POST", "/komino/api/rooms", { json: settings });
  const code = room.room.code;
  t.after(async () => {
    await guest.post(`/komino/api/rooms/${code}/leave`).then((r) => r.text());
    await host.post(`/komino/api/rooms/${code}/leave`).then((r) => r.text());
  });
  assert.deepEqual(room.room.settings, settings);
  const joined = await guest.json("POST", `/komino/api/rooms/${code}/join`);
  assert.deepEqual(joined.room.settings, settings, "a joiner sees the same settings");

  let view = await host.json("POST", `/komino/api/rooms/${code}/action`, { json: { type: "start" } });
  assert.equal(view.game.hand_size, 6);
  for (const seat of view.game.seats) assert.equal(seat.slots.length, 6);
  // 2 x 6 + 1 leaves 47 of one deck
  assert.equal(view.game.deck_count, 47);

  // the opening peek is the near row: slots 1-3
  const key = await clientKey();
  const opened = await openSealed(key, server.public_key, code, await reveal(host, code, key, "opening"));
  assert.deepEqual(opened.cards.map((c) => c.slot), [0, 1, 2]);

  await host.json("POST", `/komino/api/rooms/${code}/action`, { json: { type: "ready" } });
  view = await guest.json("POST", `/komino/api/rooms/${code}/action`, { json: { type: "ready" } });
  assert.equal(view.game.status, "playing");
  const left = view.game.turn_deadline - view.server_now;
  assert.ok(left > 50_000 && left <= 60_000, `the turn limit should be about 60s, got ${left}ms`);

  // hide is accepted from anyone seated and carries no turn token
  view = await guest.json("POST", `/komino/api/rooms/${code}/action`, { json: { type: "hide" } });
  assert.deepEqual(view.game.reveals, []);
});

test("komino holds a discarded special move for matches and takes komino mid turn", async (t) => {
  const host = new Client();
  const guest = new Client();
  await expectStatus(await host.get("/komino"), 200, "komino page");
  await expectStatus(await guest.get("/komino"), 200, "komino page");
  const server = await host.json("GET", "/komino/api/key");
  const hostMe = await host.json("POST", "/komino/api/me", { json: { name: "accept-host" } });
  const guestMe = await guest.json("POST", "/komino/api/me", { json: { name: "accept-guest" } });
  // a long away grace, since neither player holds a socket
  const room = await host.json("POST", "/komino/api/rooms", { json: { away_grace_secs: 600 } });
  const code = room.room.code;
  t.after(async () => {
    await guest.post(`/komino/api/rooms/${code}/leave`).then((r) => r.text());
    await host.post(`/komino/api/rooms/${code}/leave`).then((r) => r.text());
  });
  await guest.json("POST", `/komino/api/rooms/${code}/join`);
  const act = (c, body) => c.json("POST", `/komino/api/rooms/${code}/action`, { json: body });
  const seated = {
    [hostMe.id]: { c: host, key: await clientKey() },
    [guestMe.id]: { c: guest, key: await clientKey() },
  };
  const mover = (v) => seated[v.game.seats[v.game.turn].player];
  const other = (v) => seated[v.game.seats[1 - v.game.turn].player];

  await act(host, { type: "start" });
  await act(host, { type: "ready" });
  let view = await act(guest, { type: "ready" });

  // draw and discard until a special card comes up
  let card = null;
  for (let i = 0; i < 40 && !(card >= 7); i++) {
    const { c, key } = mover(view);
    view = await act(c, { type: "draw", turn_seq: view.game.turn_seq });
    ({ card } = await openSealed(key, server.public_key, code, await reveal(c, code, key, "drawn")));
    view = await act(c, { type: "discard", turn_seq: view.game.turn_seq });
  }
  assert.ok(card >= 7, "no special card in 40 draws");
  assert.equal(view.game.stage.kind, "earned");
  // the turn waits on its player, with the discard open to matches
  const seen = await other(view).c.json("GET", `/komino/api/rooms/${code}`);
  assert.equal(seen.game.turn, view.game.turn);
  assert.equal(seen.game.stage.kind, "earned");
  assert.equal(seen.game.matchable, true);

  // the other player matches over their socket with a reported reaction; the
  // result waits for the match window, then lands right or wrong (RT-16)
  const matcher = other(view);
  const ws = socket(matcher.c, `/komino/r/${code}/ws`);
  t.after(() => ws.close());
  const opened = await nextMessage(ws, (m) => m.type === "view");
  assert.equal(opened.view.game.discard_seq, seen.game.discard_seq);
  const sentAt = Date.now();
  ws.send(JSON.stringify({
    type: "match", ref: 1, seq: seen.game.discard_seq, seat: 1 - view.game.turn, slot: 1, reaction_ms: 300,
  }));
  const matched = await nextMessage(ws, (m) => m.type === "result" && m.ref === 1);
  assert.equal(matched.ok, true, JSON.stringify(matched));
  assert.ok(Date.now() - sentAt >= 200, "a match should settle after its window");
  view = await matcher.c.json("GET", `/komino/api/rooms/${code}`);
  assert.equal(view.events[0].kind, "match");
  assert.equal(view.game.stage.kind, "earned", "a match leaves the special move waiting");
  ws.close();

  const special = mover(view);
  view = await act(special.c, { type: "use_special", turn_seq: view.game.turn_seq });
  assert.equal(view.game.stage.kind, "special");
  view = await act(special.c, { type: "skip", turn_seq: view.game.turn_seq });
  assert.notEqual(mover(view), special, "the turn should pass");

  // everyone has played once this turn's player draws, so they may call
  const caller = mover(view);
  view = await act(caller.c, { type: "draw", turn_seq: view.game.turn_seq });
  assert.equal(view.game.can_call, true);
  view = await act(caller.c, { type: "komino", turn_seq: view.game.turn_seq });
  assert.equal(view.game.calling, true);
  assert.equal(view.game.status, "playing");
  const theirs = await special.c.json("GET", `/komino/api/rooms/${code}`);
  assert.equal(theirs.game.calling, false, "a pending call is the caller's alone");
  assert.equal(theirs.game.status, "playing");
  view = await act(caller.c, { type: "swap", slot: 0, turn_seq: view.game.turn_seq });
  assert.equal(view.game.status, "final");
  assert.equal(seated[view.game.seats[view.game.caller].player], caller);
  assert.ok(view.events.some((e) => e.kind === "komino"));
  // clients play effects for events newer than the last id they saw
  const ids = view.events.map((e) => e.id);
  assert.ok(ids.every((id) => Number.isInteger(id)), "every event carries an id");
  assert.ok(ids.every((id, i) => i === 0 || ids[i - 1] > id), "events come newest first by id");
});

test("komino rejects unknown rooms", async () => {
  const c = new Client();
  await c.json("GET", "/komino/api/rooms/ZZZZZZ/watch", {}, 404);
  await c.json("POST", "/komino/api/rooms/ZZZZZZ/join", {}, 404);
});
