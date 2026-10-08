// The watch page: a read-only live view that never joins the room.
import { test } from "node:test";
import assert from "node:assert/strict";
import { boot, makeGame, makeView } from "../helpers.mjs";

const PATH = "/komino/r/ABCDEF/watch";

function observerView(game = makeGame({ me: null }), extra = {}) {
  return makeView(game, { me: null, observer: true, observers: 1, ...extra });
}

test("watching loads the public view and the watch socket without joining", async (t) => {
  const page = await boot({ path: PATH, view: observerView() });
  t.after(page.stop);
  assert.ok(page.calls.some((c) => c.key === "GET /komino/api/rooms/ABCDEF/watch"));
  assert.equal(page.calls.some((c) => c.key.endsWith("/join")), false);
  assert.equal(page.socket.url, "wss://kominick.com/komino/r/ABCDEF/watch/ws");
  assert.ok(page.doc.body.classList.contains("watching"));
  assert.equal(page.$("join-link").getAttribute("href"), "/komino/r/ABCDEF");
  assert.equal(page.$("observers").textContent, "1 watching");
  // every seat is shown as another player's hand
  assert.equal(page.doc.querySelectorAll(".hand").length, 2);
  assert.equal(page.doc.querySelector(".hand.mine"), null);
  assert.equal(page.$("status").textContent, "ada's turn");
});

test("observers get no controls, no confirm, and no host buttons", async (t) => {
  // even the host, when watching, only watches
  const page = await boot({ path: PATH, view: observerView(makeGame({ me: null, can_call: true })) });
  t.after(page.stop);
  page.live();
  assert.deepEqual(page.controls(), []);
  // nothing to do, so no action bar either (UI-33)
  assert.equal(page.$("actionbar").hidden, true);
  assert.equal(page.$("start").hidden, true);
  assert.equal(page.$("members").querySelectorAll("button").length, 0);
  page.slot(0, 0).click();
  page.slot(1, 0).click();
  page.$("deck").click();
  page.$("discard").click();
  assert.equal(page.confirmOpen(), false);
  assert.equal(page.socket.sent.length, 0);
});

test("observers see the peek phase without anyone's cards", async (t) => {
  const page = await boot({ path: PATH, view: observerView(makeGame({ me: null, status: "peeking", ready_deadline: Date.now() + 9_500 })) });
  t.after(page.stop);
  assert.equal(page.$("status").textContent, "players are memorizing their bottom two cards. play starts in 10s or when everyone is ready.");
  assert.equal(page.doc.querySelectorAll(".table .hand svg text").length, 0);
});

test("a full room says so and does not retry", async (t) => {
  const page = await boot({ path: PATH, view: observerView() });
  t.after(page.stop);
  page.live().receive({ type: "full", message: "this room already has 4 observers" });
  assert.equal(page.$("gone").hidden, false);
  assert.equal(page.$("gone-title").textContent, "room is full");
  assert.equal(page.$("gone-text").textContent, "this room already has 4 observers. Try again later.");
  page.socket.drop();
  assert.equal(page.sockets.length, 1);
});

test("a removed player can't watch", async (t) => {
  const page = await boot({ path: PATH, routes: { "GET /komino/api/rooms/ABCDEF/watch": [403, { code: "removed" }] } });
  t.after(page.stop);
  assert.equal(page.$("gone-title").textContent, "removed");
});

test("a room with no game says so", async (t) => {
  const page = await boot({ path: PATH, view: observerView(null) });
  t.after(page.stop);
  assert.equal(page.$("status").textContent, "watching. no game yet.");
});

test("live updates re-render for observers", async (t) => {
  const page = await boot({ path: PATH, view: observerView() });
  t.after(page.stop);
  page.push(observerView(makeGame({ me: null, turn: 1, deck_count: 12 }), { observers: 4 }));
  assert.equal(page.$("status").textContent, "bob's turn");
  assert.equal(page.$("deck").getAttribute("aria-label"), "draw pile, 12 cards");
  assert.equal(page.$("observers").textContent, "4 watching");
});
