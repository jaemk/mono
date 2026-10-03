// Room chrome: socket messages, reconnects, host controls, the log, stats,
// peeks, and the end of a game.
import { test } from "node:test";
import assert from "node:assert/strict";
import { boot, flush, makeGame, makeView, member, seat } from "../helpers.mjs";

test("socket rejections become readable toasts", async (t) => {
  const page = await boot();
  t.after(page.stop);
  const s = page.live();
  s.receive({ type: "result", ok: false, code: "too_late", message: "x" });
  assert.equal(page.toast(), "too late, someone matched first");
  s.receive({ type: "result", ok: false, code: "stale", message: "x" });
  assert.equal(page.toast(), "the turn moved on; check the table and choose again");
  s.receive({ type: "result", ok: false, code: "invalid", message: "you cannot peek at that card now" });
  assert.equal(page.toast(), "you cannot peek at that card now");
  s.receive({ type: "error", code: "error", message: "database error" });
  assert.equal(page.toast(), "database error");
  page.$("toast").hidden = true;
  s.receive({ type: "result", ok: true, ref: 1 });
  assert.equal(page.toast(), null);
});

test("a removed player is shown out and the socket stays closed", async (t) => {
  const page = await boot();
  t.after(page.stop);
  page.live().receive({ type: "removed", code: "removed" });
  assert.equal(page.$("room").hidden, true);
  assert.equal(page.$("gone").hidden, false);
  assert.equal(page.$("gone-title").textContent, "removed");
  assert.equal(page.socket.closed, true);
  assert.equal(page.sockets.length, 1);
});

test("a removed player can't load the room", async (t) => {
  const page = await boot({ routes: { "POST /komino/api/rooms/ABCDEF/join": [403, { code: "removed", message: "x" }] } });
  t.after(page.stop);
  assert.equal(page.$("gone-title").textContent, "removed");
  assert.equal(page.sockets.length, 0);
});

test("an unknown room says so", async (t) => {
  const page = await boot({ routes: { "POST /komino/api/rooms/ABCDEF/join": [404, { code: "not_found" }] } });
  t.after(page.stop);
  assert.equal(page.$("gone-title").textContent, "room not found");
  assert.equal(page.$("gone-text").textContent, "There is no room with code ABCDEF.");
});

test("a dropped socket reconnects with growing backoff", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const page = await boot();
  t.after(page.stop);
  page.live().drop();
  assert.equal(page.$("status").textContent, "reconnecting...");
  assert.ok(page.$("status").classList.contains("offline"));
  t.mock.timers.tick(499);
  assert.equal(page.sockets.length, 1);
  t.mock.timers.tick(1);
  assert.equal(page.sockets.length, 2);
  page.socket.drop();
  t.mock.timers.tick(999);
  assert.equal(page.sockets.length, 2);
  t.mock.timers.tick(1);
  assert.equal(page.sockets.length, 3);
  // a good connection resets the backoff
  page.socket.open();
  assert.equal(page.$("status").classList.contains("offline"), false);
  page.socket.drop();
  t.mock.timers.tick(500);
  assert.equal(page.sockets.length, 4);
  // it never waits more than 10s
  for (let i = 0; i < 8; i++) {
    page.socket.drop();
    t.mock.timers.tick(10_000);
  }
  assert.equal(page.sockets.length, 12);
});

test("the host starts games; others don't see the button", async (t) => {
  const page = await boot({ view: makeView(null) });
  t.after(page.stop);
  assert.equal(page.$("table").textContent, "No game yet. The host starts one once at least two players are here.");
  assert.equal(page.$("start").hidden, false);
  assert.equal(page.$("start").textContent, "start game");
  page.live();
  page.$("start").click();
  assert.deepEqual(page.socket.sent, [{ ref: 1, type: "start" }]);
  page.push(makeView(makeGame()));
  assert.equal(page.$("start").hidden, true);
  page.push(makeView(makeGame({ status: "scored" })));
  assert.equal(page.$("start").hidden, false);
  assert.equal(page.$("start").textContent, "next game");
  page.push(makeView(null, { room: { ...makeView().room, host: "p1" } }));
  assert.equal(page.$("start").hidden, true);
});

test("leaving asks first, then forfeits and goes to the lobby", async (t) => {
  const page = await boot({ confirmAnswer: false, routes: { "POST /komino/api/rooms/ABCDEF/leave": [200, { ok: true }] } });
  t.after(page.stop);
  page.live();
  page.$("leave").click();
  await flush();
  assert.equal(page.confirms[0], "leave this room? if you are in a game you forfeit it.");
  assert.equal(page.calls.some((c) => c.key.endsWith("/leave")), false);

  const yes = await boot({ routes: { "POST /komino/api/rooms/ABCDEF/leave": [500, {}] } });
  t.after(yes.stop);
  yes.live();
  yes.$("leave").click();
  await flush();
  assert.ok(yes.calls.some((c) => c.key === "POST /komino/api/rooms/ABCDEF/leave"));
  assert.equal(yes.socket.closed, true);
  assert.equal(yes.location.href, "/komino");
  // a closed socket doesn't reconnect
  yes.socket.drop();
  assert.equal(yes.sockets.length, 1);
});

test("copy link and copy watch link use the clipboard, or show the link", async (t) => {
  const page = await boot();
  t.after(page.stop);
  page.$("copy").click();
  await flush();
  page.$("copy-watch").click();
  await flush();
  assert.deepEqual(page.clipboard, ["https://kominick.com/komino/r/ABCDEF", "https://kominick.com/komino/r/ABCDEF/watch"]);
  assert.equal(page.toast(), "link copied");

  const blocked = await boot({ clipboardFails: true });
  t.after(blocked.stop);
  blocked.$("copy-watch").click();
  await flush();
  assert.equal(blocked.toast(), "https://kominick.com/komino/r/ABCDEF/watch");
});

test("members show presence and tags; the host can remove and unban", async (t) => {
  const members = [
    member("p0", "ada"),
    member("p1", "bob", { present: false }),
    member("p2", "cy", { removed: true, left: true, present: false }),
    member("p3", "dee", { left: true }),
  ];
  const removedView = makeView(makeGame(), { members: [...members.slice(0, 1), member("p1", "bob", { removed: true })] });
  const page = await boot({
    view: makeView(makeGame(), { members }),
    routes: {
      "POST /komino/api/rooms/ABCDEF/remove": [200, removedView],
      "POST /komino/api/rooms/ABCDEF/unban": [403, { code: "forbidden", message: "only the host can unban players" }],
    },
  });
  t.after(page.stop);
  const rows = [...page.$("members").querySelectorAll("li")];
  assert.equal(rows.length, 3, "a player who left is hidden unless removed");
  assert.equal(rows[0].querySelector(".tag").textContent, "host, you");
  assert.ok(rows[0].querySelector(".dot.on"));
  assert.equal(rows[1].querySelector(".dot.on"), null);
  assert.equal(rows[2].querySelector(".tag").textContent, "removed");
  assert.equal(rows[0].querySelector("button"), null);

  rows[1].querySelector("button").click();
  await flush();
  assert.equal(page.confirms[0], "remove bob from the room?");
  assert.deepEqual(page.calls.at(-1), { key: "POST /komino/api/rooms/ABCDEF/remove", body: { player: "p1" } });
  // the returned view is applied
  const after = [...page.$("members").querySelectorAll("li")];
  assert.equal(after[1].querySelector("button").textContent, "unban");
  after[1].querySelector("button").click();
  await flush();
  assert.deepEqual(page.calls.at(-1), { key: "POST /komino/api/rooms/ABCDEF/unban", body: { player: "p1" } });
  assert.equal(page.toast(), "only the host can unban players");
});

test("a declined remove sends nothing, and a failed one says why", async (t) => {
  const page = await boot({ confirmAnswer: false });
  t.after(page.stop);
  page.$("members").querySelectorAll("li")[1].querySelector("button").click();
  await flush();
  assert.equal(page.calls.some((c) => c.key.endsWith("/remove")), false);

  const failing = await boot({ routes: { "POST /komino/api/rooms/ABCDEF/remove": [400, { message: "that player is not in this room" }] } });
  t.after(failing.stop);
  failing.$("members").querySelectorAll("li")[1].querySelector("button").click();
  await flush();
  assert.equal(failing.toast(), "that player is not in this room");
});

test("non-hosts get no member buttons", async (t) => {
  const page = await boot({ me: { id: "p1", name: "bob" }, view: makeView(makeGame({ me: 1 }), { me: "p1" }) });
  t.after(page.stop);
  assert.equal(page.$("members").querySelectorAll("button").length, 0);
});

test("the log describes every event kind", async (t) => {
  const kinds = [
    [{ kind: "start", player: "p0" }, "you started a game"],
    [{ kind: "ready", player: "p1" }, "bob is ready"],
    [{ kind: "play" }, "play begins"],
    [{ kind: "draw", player: "p1" }, "bob drew a card"],
    [{ kind: "take", player: "p1", payload: { value: 5 } }, "bob took the 5"],
    [{ kind: "swap", player: "p1", payload: { slot: 0, discarded: 9 } }, "bob swapped into card 1, discarding 9"],
    [{ kind: "discard", player: "p0", payload: { value: 12 } }, "you discarded 12"],
    [{ kind: "peek", player: "p1", payload: { seat: 0, slot: 3 } }, "bob peeked at your card 4"],
    [{ kind: "blind_swap", player: "p0", payload: { target_seat: 1, target_slot: 1 } }, "you blind swapped with bob's card 2"],
    [{ kind: "look_swap", player: "p1", payload: { target_seat: 9, target_slot: 0 } }, "bob swapped with a card 1"],
    [{ kind: "skip", player: "p1" }, "bob skipped the move"],
    [{ kind: "match", player: "p1", payload: { ok: true, seat: 1, value: 4 } }, "bob matched bob's 4"],
    [{ kind: "match", player: "p1", payload: { ok: false, seat: 0, slot: 2 } }, "bob missed a match on your card 3 and took a penalty"],
    [{ kind: "komino", player: "p1" }, "bob called KOMINO"],
    [{ kind: "forfeit", player: "p9" }, "someone left the game"],
    [{ kind: "away_skip", player: "p1" }, "bob was away; turn skipped"],
    [{ kind: "scored", payload: { winners: ["p0", "p1"] } }, "game over: you, bob"],
    [{ kind: "scored", payload: {} }, "game over: no winner"],
    [{ kind: "mystery" }, "mystery"],
  ];
  const events = kinds.map(([e]) => ({ player: null, payload: {}, ...e }));
  const page = await boot({ view: makeView(makeGame(), { events }) });
  t.after(page.stop);
  const lines = [...page.$("log").querySelectorAll("li")].map((li) => li.textContent);
  assert.deepEqual(lines, kinds.map(([, text]) => text));
});

test("names are rendered as text", async (t) => {
  const page = await boot({ view: makeView(makeGame(), { members: [member("p0", "ada"), member("p1", "<b>x</b>")] }) });
  t.after(page.stop);
  assert.equal(page.doc.querySelector("b"), null);
  assert.ok(page.$("members").textContent.includes("<b>x</b>"));
});

test("stats list every player, marking those who left", async (t) => {
  const empty = await boot();
  t.after(empty.stop);
  assert.equal(empty.$("stats").textContent, "no games yet");

  const row = (player, name, wins) => ({
    player, name, games_played: 2, wins, komino_calls: 1, komino_wins: 0, matches: 3,
    failed_matches: 1, special_moves: 2, cards_interacted: 9, forfeits: 0,
  });
  const page = await boot({
    view: makeView(makeGame(), {
      members: [member("p0", "ada"), member("p1", "bob", { left: true })],
      stats: [row("p0", "ada", 2), row("p1", "bob", 0)],
    }),
  });
  t.after(page.stop);
  const cells = [...page.$("stats").querySelectorAll("tr")].map((tr) => [...tr.children].map((c) => c.textContent));
  assert.deepEqual(cells[0], ["player", "games", "wins", "calls", "call wins", "matches", "missed", "specials", "cards", "forfeits"]);
  assert.deepEqual(cells[1], ["ada", "2", "2", "1", "0", "3", "1", "2", "9", "0"]);
  assert.equal(cells[2][0], "bob (gone)");
});

test("the observer count shows only when someone is watching", async (t) => {
  const page = await boot();
  t.after(page.stop);
  assert.equal(page.$("observers").hidden, true);
  page.push(makeView(makeGame(), { observers: 3 }));
  assert.equal(page.$("observers").hidden, false);
  assert.equal(page.$("observers").textContent, "3 watching");
  assert.equal(page.doc.body.classList.contains("watching"), false);
});

test("a peeked card stays up 5s or until hidden; others see the slot lit", async (t) => {
  t.mock.timers.enable({ apis: ["Date"], now: 1_000_000 });
  const reveal = { seat: 1, slot: 2, until: 1_005_000 };
  const g = makeGame({ turn: 1, matchable: false, reveals: [reveal], peeked: [reveal, { seat: 0, slot: 0, until: 1_004_000 }] });
  g.seats[1].slots[2] = { v: 9 };
  const page = await boot({ view: makeView(g) });
  t.after(page.stop);
  const peeked = (s, i) => page.slot(s, i).classList.contains("peeked");
  assert.equal(page.slot(1, 2).getAttribute("aria-label"), "bob's card 3: 9, peek other");
  assert.ok(peeked(1, 2));
  assert.ok(peeked(0, 0), "someone else's peek lights the slot");
  assert.ok(page.control("hide card"));

  page.control("hide card").click();
  assert.equal(page.slot(1, 2).getAttribute("aria-label"), "bob's card 3, face down");
  assert.equal(peeked(1, 2), false);
  assert.equal(page.control("hide card"), undefined);

  // a fresh peek flips back on its own
  const again = { seat: 1, slot: 1, until: 1_006_000 };
  const g2 = makeGame({ turn: 1, matchable: false, reveals: [again], peeked: [again] });
  g2.seats[1].slots[1] = { v: 2 };
  page.push(makeView(g2));
  assert.equal(page.slot(1, 1).getAttribute("aria-label"), "bob's card 2: 2");
  t.mock.timers.tick(5000);
  page.push(makeView(g2));
  assert.equal(page.slot(1, 1).getAttribute("aria-label"), "bob's card 2, face down");
  assert.equal(peeked(1, 1), false);
});

test("the slot light for someone else's peek fades after 5s", async (t) => {
  t.mock.timers.enable({ apis: ["Date"], now: 2_000_000 });
  const page = await boot({ view: makeView(makeGame({ turn: 1, peeked: [{ seat: 1, slot: 0, until: 2_005_000 }] })) });
  t.after(page.stop);
  assert.ok(page.slot(1, 0).classList.contains("peeked"));
  assert.equal(page.control("hide card"), undefined);
  t.mock.timers.tick(5000);
  page.push(makeView(makeGame({ turn: 1, peeked: [{ seat: 1, slot: 0, until: 2_005_000 }] })));
  assert.equal(page.slot(1, 0).classList.contains("peeked"), false);
});

test("an away player's countdown shows", async (t) => {
  const page = await boot({ view: makeView(makeGame({ turn: 1, away_deadline: Date.now() + 29_200 })) });
  t.after(page.stop);
  assert.equal(page.$("status").textContent, "bob's turn (away, skipping in 30s)");
});

test("scoring flips every hand and names the winners", async (t) => {
  const g = makeGame({
    status: "scored",
    seats: [seat("p0", [1, 2, 3, 4], { score: 10, won: false }), seat("p1", [0, 0, 1, -1], { score: 0, won: true })],
  });
  const page = await boot({ view: makeView(g) });
  t.after(page.stop);
  assert.equal(page.doc.querySelectorAll(".table .card svg text.corner").length, 2 * 9, "every hand and the discard face up");
  assert.equal(page.$("status").textContent, "game over. winner: bob");
  const scores = [...page.doc.querySelectorAll(".score")].map((s) => s.textContent);
  assert.deepEqual(scores, ["0 won", "10"]);
  assert.ok(page.doc.querySelector(".score.won"));
  assert.equal(page.controls().includes("KOMINO"), true);

  g.seats[0].won = true;
  page.push(makeView(g));
  assert.equal(page.$("status").textContent, "game over. winner: you, bob");
  g.seats.forEach((s) => (s.won = false));
  page.push(makeView(g));
  assert.equal(page.$("status").textContent, "game over");
});

test("countdowns tick down while a game runs", async (t) => {
  t.mock.timers.enable({ apis: ["setInterval", "Date"], now: 3_000_000 });
  const page = await boot({ view: makeView(makeGame({ status: "peeking", ready_deadline: 3_030_000 })) });
  t.after(page.stop);
  assert.match(page.$("status").textContent, /play starts in 30s/);
  t.mock.timers.tick(1000);
  assert.match(page.$("status").textContent, /play starts in 29s/);
  t.mock.timers.tick(40_000);
  assert.match(page.$("status").textContent, /play starts in 0s/);
});
