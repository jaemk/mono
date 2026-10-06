import { test } from "node:test";
import assert from "node:assert/strict";
import { boot, flush, makeGame, makeView, seat } from "../helpers.mjs";

const created = { "POST /komino/api/rooms": [200, { room: { code: "QWERTY" } }] };

test("create sends the default settings", async (t) => {
  const page = await boot({ path: "/komino", routes: created });
  t.after(page.stop);
  page.$("create").click();
  await flush();
  assert.deepEqual(page.callsTo("POST /komino/api/rooms")[0].body,
    { hand_size: 4, turn_limit_secs: null, away_grace_secs: 30, reveal_secs: null });
  assert.equal(page.location.href, "/komino/r/QWERTY");
});

test("create sends the chosen settings", async (t) => {
  const page = await boot({ path: "/komino", routes: created });
  t.after(page.stop);
  page.$("set-hand").value = "9";
  page.$("set-turn").value = "90";
  page.$("set-away").value = "120";
  page.$("set-reveal").value = "5";
  page.$("create").click();
  await flush();
  assert.deepEqual(page.callsTo("POST /komino/api/rooms")[0].body,
    { hand_size: 9, turn_limit_secs: 90, away_grace_secs: 120, reveal_secs: 5 });
});

test("the room shows its settings", async (t) => {
  const settings = { hand_size: 6, away_grace_secs: 30, turn_limit_secs: 60, reveal_secs: null };
  const view = makeView();
  view.room.settings = settings;
  const page = await boot({ view });
  t.after(page.stop);
  assert.equal(page.$("settings").textContent, "6 cards, 60s turns, 30s away grace, peeks until hidden");
  const other = makeView();
  other.room.settings = { hand_size: 4, away_grace_secs: 15, turn_limit_secs: null, reveal_secs: 10 };
  page.push(other);
  assert.equal(page.$("settings").textContent, "4 cards, no turn limit, 15s away grace, 10s peeks");
  // a view without settings shows nothing
  page.push(makeView());
  assert.equal(page.$("settings").textContent, "");
});

test("hands lay out in two rows, wide ones marked to shrink", async (t) => {
  const nine = ["x", "x", "x", "x", "x", "x", "x", "x", "x"];
  const g = makeGame({ hand_size: 9, seats: [seat("p0", nine), seat("p1", nine)] });
  const page = await boot({ view: makeView(g) });
  t.after(page.stop);
  const grids = [...page.doc.querySelectorAll(".slots")];
  assert.equal(grids.length, 2);
  for (const grid of grids) {
    assert.equal(grid.style.getPropertyValue("--cols").trim(), "5");
    assert.ok(grid.classList.contains("wide"));
    assert.equal(grid.querySelectorAll(".card").length, 9);
  }
  // a game without hand_size is a 4 card game
  page.push(makeView(makeGame()));
  const grid = page.doc.querySelector(".slots");
  assert.equal(grid.style.getPropertyValue("--cols").trim(), "2");
  assert.equal(grid.classList.contains("wide"), false);
});

test("other players' hands are turned toward them; yours and an observer's are not (UI-30)", async (t) => {
  const at = (page, s, n) => page.slot(s, n).getAttribute("style");
  const page = await boot();
  t.after(page.stop);
  // bob's near row (cards 3 and 4) is farthest away, his card 1 bottom right
  assert.deepEqual([0, 1, 2, 3].map((n) => at(page, 1, n)), ["grid-area: 2 / 2", "grid-area: 2 / 1", "grid-area: 1 / 2", "grid-area: 1 / 1"]);
  assert.deepEqual([0, 1, 2, 3].map((n) => at(page, 0, n)), [null, null, null, null]);
  // an odd hand leaves its gap at the far left
  page.push(makeView(makeGame({ hand_size: 5, seats: [seat("p0", Array(5).fill("x")), seat("p1", Array(5).fill("x"))] })));
  assert.deepEqual([0, 2, 3, 4].map((n) => at(page, 1, n)), ["grid-area: 2 / 3", "grid-area: 2 / 1", "grid-area: 1 / 3", "grid-area: 1 / 2"]);
  // a penalty card adds a row on bob's side, farthest away
  page.push(makeView(makeGame({ seats: [seat("p0"), seat("p1", ["x", "x", "x", "x", "x"])] })));
  assert.equal(at(page, 1, 4), "grid-area: 1 / 2");
  assert.equal(at(page, 1, 0), "grid-area: 3 / 2");

  const watch = await boot({ path: "/komino/r/ABCDEF/watch", view: makeView(makeGame({ me: null }), { me: null, observer: true }) });
  t.after(watch.stop);
  assert.deepEqual([0, 1].map((s) => at(watch, s, 0)), [null, null]);
});

test("every card shows its slot number, face up, face down, or empty (UI-31)", async (t) => {
  const page = await boot({ view: makeView(makeGame({ seats: [seat("p0", [5, "x", null, "x"]), seat("p1")] })) });
  t.after(page.stop);
  const no = (el) => el.querySelector(".slot-no").textContent;
  assert.deepEqual([0, 1, 3].map((n) => no(page.slot(0, n))), ["1", "2", "4"]);
  assert.equal(no(page.doc.querySelector("[data-seat='0'][data-slot='2']")), "3");
  assert.deepEqual([0, 1, 2, 3].map((n) => no(page.slot(1, n))), ["1", "2", "3", "4"]);
  assert.equal(page.slot(0, 0).classList.contains("down"), false);
  assert.ok(page.slot(0, 1).classList.contains("down"));
  assert.equal(page.$("deck").querySelector(".slot-no"), null);
});

test("the opening peek names the near row's size", async (t) => {
  const ten = Array(10).fill("x");
  const g = makeGame({ status: "peeking", hand_size: 10, ready_deadline: Date.now() + 30_000, seats: [seat("p0", ten), seat("p1", ten)] });
  const page = await boot({ view: makeView(g) });
  t.after(page.stop);
  assert.match(page.$("status").textContent, /^memorize your bottom 5 cards\./);
  page.push(makeView(makeGame({ status: "peeking", hand_size: 5, ready_deadline: Date.now() + 30_000 }), { me: "p0" }));
  assert.match(page.$("status").textContent, /^memorize your bottom two cards\./);
});

test("a turn limit counts down in the status line", async (t) => {
  t.mock.timers.enable({ apis: ["Date"], now: 8_000_000 });
  const page = await boot({
    view: makeView(makeGame({ turn: 1, turn_deadline: 8_045_000, away_deadline: 8_020_000 }), { server_now: 8_000_000 }),
  });
  t.after(page.stop);
  assert.equal(page.$("status").textContent, "bob's turn (45s left) (away, skipping in 20s)");
});

test("a peek without a deadline stays up until hidden", async (t) => {
  t.mock.timers.enable({ apis: ["Date"], now: 9_000_000 });
  const reveal = { id: "r5", seat: 1, slot: 0, until: null };
  const g = makeGame({ turn: 1, matchable: false, reveals: [reveal], peeked: [reveal] });
  const page = await boot({ view: makeView(g), secrets: { "peek:r5": { cards: [{ seat: 1, slot: 0, v: 3 }] } } });
  t.after(page.stop);
  await page.settle(() => page.label(1, 0) === "bob's card 1: 3");
  t.mock.timers.tick(10 * 60 * 1000);
  page.push(makeView(g));
  assert.equal(page.label(1, 0), "bob's card 1: 3");
  assert.ok(page.slot(1, 0).classList.contains("peeked"));
  page.live();
  page.control("hide card").click();
  assert.equal(page.label(1, 0), "bob's card 1, face down");
  assert.equal(page.socket.sent.at(-1).type, "hide");
});

test("someone else's untimed peek lights the slot until it ends", async (t) => {
  const peek = { seat: 1, slot: 3, until: null };
  const page = await boot({ view: makeView(makeGame({ turn: 1, peeked: [peek] })) });
  t.after(page.stop);
  assert.ok(page.slot(1, 3).classList.contains("peeked"));
  assert.equal(page.control("hide card"), undefined);
  page.push(makeView(makeGame({ turn: 1, peeked: [] })));
  assert.equal(page.slot(1, 3).classList.contains("peeked"), false);
});

test("a peek whose value was lost to a reload can still be hidden", async (t) => {
  const reveal = { id: "r6", seat: 1, slot: 2, until: null };
  const page = await boot({
    view: makeView(makeGame({ turn: 1, reveals: [reveal], peeked: [reveal] })),
    secrets: { "peek:r6": [409, { code: "already_revealed", message: "that peek was already revealed" }] },
  });
  t.after(page.stop);
  await page.settle(() => page.reveals().length === 1);
  await page.idle();
  assert.equal(page.label(1, 2), "bob's card 3, face down");
  page.live();
  page.control("hide card").click();
  assert.equal(page.socket.sent.at(-1).type, "hide");
});
