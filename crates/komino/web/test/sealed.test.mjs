// Sealed reveals (spec SEAL-*): private values arrive encrypted to a
// short-lived, non-extractable client key and are dropped once hidden.
import { test } from "node:test";
import assert from "node:assert/strict";
import { webcrypto, createECDH } from "node:crypto";
import { boot, komino, makeGame, makeView, seat } from "../helpers.mjs";

const { openSealed, importServerKey } = komino;
const subtle = webcrypto.subtle;
const KEY = "GET /komino/api/key";

// the vector crates/komino/src/sealed.rs asserts it produces
const VECTOR = {
  serverPublic: "BGD-1LolWp0xyWHrdMY1bWjASbiSO2H6bOZpYi5g8p-2eQP-EAi4vJmkGunpVii8ZPLxsgwtfp9Rd6PClNRGIpk",
  clientD: "519b423d715f8b581f4fa8ee59f4771a5b44c8130b4e3eacca54a56dda72b464",
  sealed: {
    kid: "b18b86ce1389e46d",
    salt: "BwcHBwcHBwcHBwcHBwcHBw",
    iv: "CQkJCQkJCQkJCQkJ",
    ct: "BOIuu3b34FzCYk9QlGtjjUQIf33O57vqUZnIEvrFGz7UQ-JHHxmKNDNTmK9k1DoEV9_BBYY",
  },
};

async function vectorClientKey() {
  const d = Buffer.from(VECTOR.clientD, "hex");
  const ecdh = createECDH("prime256v1");
  ecdh.setPrivateKey(d);
  const pub = ecdh.getPublicKey();
  const jwk = {
    kty: "EC", crv: "P-256", d: d.toString("base64url"),
    x: pub.subarray(1, 33).toString("base64url"), y: pub.subarray(33).toString("base64url"),
  };
  return subtle.importKey("jwk", jwk, { name: "ECDH", namedCurve: "P-256" }, false, ["deriveBits"]);
}

test("the browser opens what the rust server seals", async () => {
  const server = await importServerKey(subtle, VECTOR.serverPublic);
  const client = await vectorClientKey();
  const plain = await openSealed(subtle, server, client, "ABCDEF", VECTOR.sealed);
  assert.deepEqual(plain, { cards: [{ seat: 0, slot: 2, v: 5 }] });
  // bound to the room code, and to the exact bytes
  await assert.rejects(openSealed(subtle, server, client, "ZZZZZZ", VECTOR.sealed));
  await assert.rejects(openSealed(subtle, server, client, "ABCDEF", { ...VECTOR.sealed, iv: "CQkJCQkJCQkJCQkK" }));
});

test("the opening peek is revealed sealed and dropped at ready", async (t) => {
  const g = makeGame({ status: "peeking", ready_deadline: Date.now() + 20_000 });
  const page = await boot({
    view: makeView(g),
    secrets: { opening: { cards: [{ seat: 0, slot: 2, v: 5 }, { seat: 0, slot: 3, v: -1 }] } },
  });
  t.after(page.stop);
  await page.settle(() => page.label(0, 3) === "your card 4: -1");
  assert.equal(page.label(0, 2), "your card 3: 5");
  assert.equal(page.label(0, 0), "your card 1, face down");
  assert.equal(page.label(1, 2), "bob's card 3, face down");
  // the request carries only the client's public key and what is wanted
  const [req] = page.reveals();
  assert.deepEqual(Object.keys(req).sort(), ["client_key", "what"]);
  assert.equal(req.what, "opening");
  // the response logged by the network layer is ciphertext
  assert.equal(JSON.stringify(page.callsTo(KEY)).includes("\"v\""), false);

  const ready = makeGame({ status: "peeking", ready_deadline: Date.now() + 20_000 });
  ready.seats[0].ready = true;
  page.push(makeView(ready));
  assert.equal(page.label(0, 2), "your card 3, face down");
  assert.equal(page.label(0, 3), "your card 4, face down");
});

test("a lower-case room link still opens reveals sealed to the canonical code", async (t) => {
  const g = makeGame({ status: "peeking", ready_deadline: Date.now() + 20_000 });
  const page = await boot({
    path: "/komino/r/abcdef",
    view: makeView(g),
    secrets: { opening: { cards: [{ seat: 0, slot: 2, v: 5 }, { seat: 0, slot: 3, v: -1 }] } },
  });
  t.after(page.stop);
  await page.settle(() => page.label(0, 3) === "your card 4: -1");
  assert.equal(page.label(0, 2), "your card 3: 5");
});

test("the server key is fetched once and the client key reused for 5 minutes", async (t) => {
  t.mock.timers.enable({ apis: ["Date"], now: 10_000_000 });
  const secrets = {
    "peek:a": { cards: [{ seat: 1, slot: 0, v: 1 }] },
    "peek:b": { cards: [{ seat: 1, slot: 1, v: 2 }] },
    "peek:c": { cards: [{ seat: 1, slot: 2, v: 3 }] },
  };
  const peek = (id, slot) => ({ id, seat: 1, slot, until: Date.now() + 5000 });
  const page = await boot({ view: makeView(makeGame({ turn: 1, reveals: [peek("a", 0)] })), secrets });
  t.after(page.stop);
  await page.settle(() => page.label(1, 0) === "bob's card 1: 1");
  page.push(makeView(makeGame({ turn: 1, reveals: [peek("b", 1)] })));
  await page.settle(() => page.label(1, 1) === "bob's card 2: 2");
  t.mock.timers.tick(5 * 60 * 1000);
  page.push(makeView(makeGame({ turn: 1, reveals: [peek("c", 2)] })));
  await page.settle(() => page.label(1, 2) === "bob's card 3: 3");
  const [a, b, c] = page.reveals().map((r) => r.client_key);
  assert.equal(a, b, "same key inside the window");
  assert.notEqual(b, c, "a new key after five minutes");
  assert.equal(page.callsTo(KEY).length, 1);
});

test("client private keys are generated non-extractable", async (t) => {
  const made = [];
  const spy = new Proxy(subtle, {
    get(target, prop) {
      const value = target[prop];
      if (prop === "generateKey") {
        return (...args) => {
          made.push(args);
          return value.apply(target, args);
        };
      }
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
  const page = await boot({
    subtle: spy,
    view: makeView(makeGame({ status: "peeking" })),
    secrets: { opening: { cards: [] } },
  });
  t.after(page.stop);
  await page.settle(() => made.length === 1);
  const [algorithm, extractable, usages] = made[0];
  assert.deepEqual(algorithm, { name: "ECDH", namedCurve: "P-256" });
  assert.equal(extractable, false);
  assert.deepEqual(usages, ["deriveBits"]);
});

test("a changed server key is fetched again and the decrypt retried", async (t) => {
  const reveal = { id: "k1", seat: 1, slot: 3, until: Date.now() + 60_000 };
  const page = await boot({ secrets: { "peek:k1": { cards: [{ seat: 1, slot: 3, v: 13 }] } } });
  t.after(page.stop);
  assert.equal(page.callsTo(KEY).length, 1);
  await page.rotateServerKey();
  page.push(makeView(makeGame({ turn: 1, reveals: [reveal] })));
  await page.settle(() => page.label(1, 3) === "bob's card 4: 13, look and swap");
  assert.equal(page.callsTo(KEY).length, 2);
  // one reveal: the retry decrypts the same response, the peek is not re-fetched
  assert.equal(page.reveals().length, 1);
});

test("a response that still won't decrypt shows an error", async (t) => {
  const reveal = { id: "bad", seat: 1, slot: 0, until: Date.now() + 60_000 };
  const junk = { kid: "x", salt: "AAAAAAAAAAAAAAAAAAAAAA", iv: "AAAAAAAAAAAAAAAA", ct: "AAAAAAAAAAAAAAAAAAAAAA" };
  const page = await boot({ secrets: { "peek:bad": [200, junk] } });
  t.after(page.stop);
  page.push(makeView(makeGame({ turn: 1, reveals: [reveal] })));
  await page.settle(() => page.toast() !== null);
  assert.equal(page.callsTo(KEY).length, 2);
  assert.equal(page.label(1, 0), "bob's card 1, face down");
});

test("an expired client key is replaced and the reveal retried once", async (t) => {
  let first = true;
  const page = await boot({
    view: makeView(makeGame({ status: "peeking" })),
    secrets: {
      opening: () => {
        if (first) {
          first = false;
          return [409, { code: "key_expired", message: "the client key expired; make a new one" }];
        }
        return { cards: [{ seat: 0, slot: 2, v: 6 }] };
      },
    },
  });
  t.after(page.stop);
  await page.settle(() => page.label(0, 2) === "your card 3: 6");
  const [a, b] = page.reveals().map((r) => r.client_key);
  assert.ok(a && b);
  assert.notEqual(a, b);
  assert.equal(page.toast(), null);
});

test("a failed opening reveal is asked for again on the next update", async (t) => {
  let fail = true;
  const g = makeGame({ status: "peeking" });
  const page = await boot({
    view: makeView(g),
    secrets: { opening: () => (fail ? [500, { message: "database error" }] : { cards: [{ seat: 0, slot: 3, v: 0 }] }) },
  });
  t.after(page.stop);
  await page.settle(() => page.toast() === "database error");
  fail = false;
  page.push(makeView(makeGame({ status: "peeking", deck_count: 40 })));
  await page.settle(() => page.label(0, 3) === "your card 4: 0");
  assert.equal(page.reveals().length, 2);
});

test("a server key that fails to load at start is tried again at the first reveal", async (t) => {
  const page = await boot({
    view: makeView(makeGame({ status: "peeking" })),
    routes: { [KEY]: [503, { message: "down" }] },
    secrets: { opening: { cards: [{ seat: 0, slot: 2, v: 4 }] } },
  });
  t.after(page.stop);
  assert.equal(page.$("room").hidden, false, "the page still loads");
  // still down when the reveal arrives, so it reports the failure
  await page.settle(() => page.toast() === "down");
  assert.equal(page.callsTo(KEY).length, 2);
  assert.equal(page.label(0, 2), "your card 3, face down");
});

test("watchers and spectators never ask for a reveal or the key", async (t) => {
  const watch = await boot({
    path: "/komino/r/ABCDEF/watch",
    view: makeView(makeGame({ me: null, status: "peeking", reveals: [] }), { me: null, observer: true }),
  });
  t.after(watch.stop);
  await watch.idle();
  assert.equal(watch.reveals().length, 0);
  assert.equal(watch.callsTo(KEY).length, 0);

  const spectator = await boot({ view: makeView(makeGame({ me: null, status: "peeking" })) });
  t.after(spectator.stop);
  await spectator.idle();
  assert.equal(spectator.reveals().length, 0);
});

test("nothing is written to browser storage or cookies", async (t) => {
  const page = await boot({
    view: makeView(makeGame({ status: "peeking", seats: [seat("p0"), seat("p1")] })),
    secrets: { opening: { cards: [{ seat: 0, slot: 2, v: 8 }] } },
  });
  t.after(page.stop);
  await page.settle(() => page.label(0, 2) === "your card 3: 8, peek own");
  assert.equal(page.w.localStorage.length, 0);
  assert.equal(page.w.sessionStorage.length, 0);
  assert.equal(page.doc.cookie, "");
});
