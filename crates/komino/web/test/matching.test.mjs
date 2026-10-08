// Matching: two taps on any card; a right match on another player's card
// then asks for one of yours to give.
import { test } from "node:test";
import assert from "node:assert/strict";
import { boot, makeGame, makeView, seat } from "../helpers.mjs";

async function waiting(t, game = {}, secrets = {}) {
  const page = await boot({ view: makeView(makeGame({ turn: 1, ...game })), secrets });
  t.after(page.stop);
  page.live();
  return page;
}

test("matching your own card takes two taps and carries the discard sequence", async (t) => {
  const page = await waiting(t);
  assert.equal(page.$("discard").nextElementSibling.textContent, "matchable");
  page.slot(0, 2).click();
  assert.equal(page.confirmText(), "match your card 3 with the 4?");
  assert.ok(page.slot(0, 2).classList.contains("sel"));
  page.tick(420);
  page.ok();
  assert.deepEqual(page.socket.sent, [{ ref: 1, type: "match", seq: 2, seat: 0, slot: 2, reaction_ms: 420 }]);
});

test("a match reports how long since this page first showed its discard", async (t) => {
  const page = await waiting(t);
  page.tick(300);
  // a later view of the same discard doesn't restart the clock
  page.push(makeView(makeGame({ turn: 1 })));
  page.tick(200);
  page.slot(0, 0).click();
  page.ok();
  assert.equal(page.socket.sent[0].reaction_ms, 500);
  // a new discard does
  page.push(makeView(makeGame({ turn: 1, discard_seq: 3 })));
  page.tick(150);
  page.slot(0, 1).click();
  page.ok();
  assert.equal(page.socket.sent[1].reaction_ms, 150);
});

test("a sent match is marked until its result arrives", async (t) => {
  const page = await waiting(t);
  page.slot(0, 2).click();
  page.ok();
  assert.ok(page.slot(0, 2).classList.contains("claiming"));
  assert.match(page.label(0, 2), /, matching$/);
  assert.ok(!page.slot(0, 1).classList.contains("claiming"));
  // another view while the window is open keeps the mark
  page.push(makeView(makeGame({ turn: 1 })));
  assert.ok(page.slot(0, 2).classList.contains("claiming"));
  page.socket.receive({ type: "result", ref: 1, ok: false, code: "too_late", message: "that discard was already matched or covered" });
  assert.ok(!page.slot(0, 2).classList.contains("claiming"));
  assert.equal(page.toast(), "too late, someone matched first");
});

test("a pending match mark clears when its discard changes", async (t) => {
  const page = await waiting(t);
  page.slot(0, 2).click();
  page.ok();
  page.socket.receive({ type: "result", ref: 99, ok: true });
  assert.ok(page.slot(0, 2).classList.contains("claiming"), "another action's result leaves it");
  page.push(makeView(makeGame({ turn: 1, discard_seq: 3 })));
  assert.ok(!page.slot(0, 2).classList.contains("claiming"));
});

test("matching another player's card confirms at once, with no card to give yet", async (t) => {
  const page = await waiting(t);
  page.slot(1, 1).click();
  assert.equal(page.confirmText(), "match bob's card 2 with the 4? if you're right, you then give them one of your cards.");
  assert.ok(page.slot(1, 1).classList.contains("sel"));
  page.ok();
  assert.deepEqual(page.socket.sent, [{ ref: 1, type: "match", seq: 2, seat: 1, slot: 1, reaction_ms: 0 }]);
});

/** A right match on bob's card 2 that ada still owes a card for. */
const owedGame = (extra = {}) => makeGame({
  turn: 1,
  discard_seq: 3,
  seats: [seat("p0"), seat("p1", ["x", null, "x", "x"])],
  owed: [{ from: 0, seat: 1, slot: 1, deadline: Date.now() + 14500 }],
  ...extra,
});

test("after a right match on another hand, you pick the card to give", async (t) => {
  const page = await waiting(t);
  page.push(makeView(owedGame()));
  assert.equal(page.toast(), "your match was right: now tap one of your cards to give");
  assert.equal(page.$("prompt").textContent,
    "your match was right: tap one of your cards to give to bob's card 2 (15s, then your highest card is given)");
  assert.ok(page.$("prompt").classList.contains("owe"));
  const target = page.doc.querySelector('[data-seat="1"][data-slot="1"]');
  assert.ok(target.classList.contains("owed"));
  assert.equal(target.getAttribute("aria-label"), "bob's card 2, waiting for your card");
  // other hands are not matches until the card is given
  page.slot(1, 0).click();
  assert.equal(page.confirmOpen(), false);
  assert.equal(page.toast(), "tap one of your own cards to give");
  page.doc.querySelector('.mine [data-slot="2"]').click();
  assert.equal(page.confirmText(), "give your card 3 to bob's card 2?");
  assert.ok(page.slot(0, 2).classList.contains("sel"));
  page.ok();
  assert.deepEqual(page.socket.sent, [{ ref: 1, type: "give", slot: 2 }]);
  // given: taps are matches again
  page.push(makeView(owedGame({ owed: [], seats: [seat("p0", ["x", "x", null, "x"]), seat("p1")] })));
  assert.equal(page.$("prompt").textContent, "");
  page.slot(1, 0).click();
  assert.match(page.confirmText(), /^match bob's card 1 with the 4\?/);
});

test("an empty slot can't be given, and an open give closes once given for you", async (t) => {
  const page = await waiting(t, { seats: [seat("p0", ["x", null, "x", "x"]), seat("p1", [null, "x", "x", "x"])],
    owed: [{ from: 0, seat: 1, slot: 0, deadline: Date.now() + 9000 }] });
  page.doc.querySelector(".mine .card.empty").click();
  assert.equal(page.confirmOpen(), false);
  page.slot(0, 0).click();
  assert.equal(page.confirmText(), "give your card 1 to bob's card 1?");
  // the deadline passed and the server gave one for you
  page.push(makeView(makeGame({ turn: 1, seats: [seat("p0", [null, null, "x", "x"]), seat("p1")] })));
  assert.equal(page.confirmOpen(), false);
  assert.equal(page.toast(), "that move is no longer available");
});

test("everyone else sees the slot waiting for the matcher's card", async (t) => {
  const page = await waiting(t, { seats: [seat("p0", [null, "x", "x", "x"]), seat("p1")],
    owed: [{ from: 1, seat: 0, slot: 0, deadline: Date.now() + 9000 }] });
  const target = page.doc.querySelector('.mine [data-slot="0"]');
  assert.equal(target.getAttribute("aria-label"), "your card 1, waiting for bob's card");
  assert.equal(page.$("prompt").textContent, "");
  // your own taps still match
  page.slot(0, 1).click();
  assert.equal(page.confirmText(), "match your card 2 with the 4?");
});

test("with no cards left you can only match your own", async (t) => {
  const page = await waiting(t, { seats: [seat("p0", [null, null, null, null]), seat("p1")] });
  page.slot(1, 0).click();
  assert.equal(page.toast(), "you have no card to give");
});

test("nothing is matchable before the first discard or after a forfeit", async (t) => {
  const page = await waiting(t, { matchable: false });
  assert.equal(page.$("discard").nextElementSibling.textContent, "discard");
  page.slot(0, 0).click();
  page.slot(1, 0).click();
  assert.equal(page.confirmOpen(), false);
  assert.equal(page.toast(), null);
  page.push(makeView(makeGame({ turn: 1, seats: [seat("p0", [], { forfeited: true }), seat("p1")] })));
  page.slot(1, 0).click();
  assert.equal(page.toast(), null);
});

test("matches stay open while scoring", async (t) => {
  const page = await waiting(t, { status: "scoring", score_at: Date.now() + 1500 });
  assert.equal(page.$("status").textContent, "scoring in 2s, last chance to match");
  page.slot(0, 0).click();
  assert.equal(page.confirmText(), "match your card 1 with the 4?");
});

test("on your turn after drawing, match mode turns taps into matches", async (t) => {
  const page = await waiting(t, { turn: 0, stage: { kind: "drawn", card: null } }, { drawn: { card: 3 } });
  await page.settle(() => page.control("discard 3"));
  assert.deepEqual(page.controls(), ["discard 3", "match 4", "KOMINO"]);
  page.slot(0, 0).click();
  assert.equal(page.confirmText(), "swap the 3 into your card 1?");
  page.control("match 4").click();
  assert.equal(page.confirmOpen(), false);
  assert.equal(page.toast(), "tap a card you think is a 4");
  assert.ok(page.control("cancel match"));
  page.slot(0, 0).click();
  assert.equal(page.confirmText(), "match your card 1 with the 4?");
  page.$("confirm-cancel").click();
  assert.ok(page.control("match 4"), "cancel leaves match mode");
  page.control("match 4").click();
  page.control("cancel match").click();
  page.slot(0, 0).click();
  assert.equal(page.confirmText(), "swap the 3 into your card 1?");
});

test("the match button is not offered before drawing", async (t) => {
  const page = await waiting(t, { turn: 0 });
  assert.equal(page.control("match 4"), undefined);
});
