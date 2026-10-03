// Matching: two taps on your own card, three when giving one of yours for
// another player's card.
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
  page.ok();
  assert.deepEqual(page.socket.sent, [{ ref: 1, type: "match", seq: 2, seat: 0, slot: 2 }]);
});

test("matching another player's card asks which of yours to give", async (t) => {
  const page = await waiting(t);
  page.slot(1, 1).click();
  assert.equal(page.confirmOpen(), false);
  assert.equal(page.toast(), "now tap one of your cards to give");
  assert.ok(page.slot(1, 1).classList.contains("sel"));
  page.slot(1, 0).click();
  assert.equal(page.toast(), "tap one of your own cards to give");
  page.slot(0, 3).click();
  assert.equal(page.confirmText(), "match bob's card 2 with the 4, giving your card 4?");
  assert.ok(page.slot(0, 3).classList.contains("sel"));
  page.ok();
  assert.deepEqual(page.socket.sent, [{ ref: 1, type: "match", seq: 2, seat: 1, slot: 1, give_slot: 3 }]);
});

test("an empty slot can't be given", async (t) => {
  const page = await waiting(t, { seats: [seat("p0", ["x", null, "x", "x"]), seat("p1")] });
  page.slot(1, 0).click();
  page.doc.querySelector(".mine .card.empty").click();
  assert.equal(page.confirmOpen(), false);
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
