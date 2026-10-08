// Turn actions: every card action is select, then confirm in the centered
// dialog, and only a confirm sends anything.
import { test } from "node:test";
import assert from "node:assert/strict";
import { boot, makeGame, makeView, seat } from "../helpers.mjs";

async function playing(t, game = {}, extra = {}, secrets = {}) {
  const page = await boot({ view: makeView(makeGame(game), extra), secrets });
  t.after(page.stop);
  page.live();
  return page;
}

test("joining loads the room and opens its socket", async (t) => {
  const page = await playing(t);
  assert.ok(page.calls.some((c) => c.key === "POST /komino/api/rooms/ABCDEF/join"));
  assert.equal(page.$("code").textContent, "ABCDEF");
  assert.equal(page.socket.url, "wss://kominick.com/komino/r/ABCDEF/ws");
  assert.equal(page.$("status").textContent, "your turn");
  // what to do next sits above the buttons in the action bar (UI-33)
  assert.equal(page.$("prompt").textContent, "draw from the deck or take the discard");
  assert.equal(page.$("actionbar").hidden, false);
  assert.equal(page.doc.querySelectorAll(".hand").length, 2);
  assert.ok(page.doc.querySelector(".hand.mine.turn"));
});

test("drawing opens the confirm dialog with focus on confirm, and confirm sends it once", async (t) => {
  const page = await playing(t);
  assert.equal(page.confirmOpen(), false);
  page.$("deck").click();
  assert.equal(page.confirmOpen(), true);
  assert.equal(page.confirmText(), "draw from the deck?");
  assert.equal(page.doc.activeElement, page.$("confirm-ok"));
  assert.equal(page.socket.sent.length, 0);
  page.ok();
  assert.equal(page.confirmOpen(), false);
  assert.deepEqual(page.socket.sent, [{ ref: 1, type: "draw", turn_seq: 5 }]);
  // a second confirm has nothing selected
  page.ok();
  assert.equal(page.socket.sent.length, 1);
});

test("cancel, escape, and a tap on the backdrop all cancel; a tap inside does not", async (t) => {
  const page = await playing(t);
  const open = () => {
    page.control("draw").click();
    assert.equal(page.confirmOpen(), true);
  };
  open();
  page.$("confirm-cancel").click();
  assert.equal(page.confirmOpen(), false);
  open();
  page.doc.dispatchEvent(new page.w.KeyboardEvent("keydown", { key: "Escape" }));
  assert.equal(page.confirmOpen(), false);
  open();
  page.doc.dispatchEvent(new page.w.KeyboardEvent("keydown", { key: "a" }));
  assert.equal(page.confirmOpen(), true);
  page.$("confirm-text").dispatchEvent(new page.w.MouseEvent("click", { bubbles: true }));
  assert.equal(page.confirmOpen(), true);
  page.$("confirm").dispatchEvent(new page.w.MouseEvent("click", { bubbles: true }));
  assert.equal(page.confirmOpen(), false);
  assert.equal(page.socket.sent.length, 0);
});

test("the discard can be taken from the pile or the button", async (t) => {
  const page = await playing(t);
  page.$("discard").click();
  assert.equal(page.confirmText(), "take the 4 from the discard pile?");
  page.$("confirm-cancel").click();
  assert.deepEqual(page.controls(), ["draw", "take 4", "KOMINO"]);
  page.control("take 4").click();
  page.ok();
  assert.deepEqual(page.socket.sent, [{ ref: 1, type: "take", turn_seq: 5 }]);
});

test("the deck and discard do nothing off turn or mid turn", async (t) => {
  const page = await playing(t, { turn: 1 });
  page.$("deck").click();
  page.$("discard").click();
  assert.equal(page.confirmOpen(), false);
  page.push(makeView(makeGame({ stage: { kind: "drawn", card: 3 } })));
  page.$("deck").click();
  assert.equal(page.confirmOpen(), false);
  page.push(makeView(makeGame({ discard_top: null, matchable: false })));
  assert.equal(page.$("discard").getAttribute("aria-label"), "discard pile, empty");
  page.$("discard").click();
  assert.equal(page.confirmOpen(), false);
  assert.deepEqual(page.controls(), ["draw", "KOMINO"]);
});

test("a drawn card is revealed sealed to the drawer, then swapped or discarded", async (t) => {
  // views never carry the drawn card; it comes from a sealed reveal
  const page = await playing(t, { stage: { kind: "drawn", card: null } }, {}, { drawn: { card: 8 } });
  await page.settle(() => page.$("drawn"));
  assert.deepEqual(page.reveals().map((r) => r.what), ["drawn"]);
  assert.ok(page.$("drawn").querySelector("svg"));
  assert.equal(page.$("prompt").textContent, "tap one of your cards to swap, or discard");
  page.slot(0, 2).click();
  assert.equal(page.confirmText(), "swap the 8 into your card 3?");
  assert.ok(page.slot(0, 2).classList.contains("sel"));
  page.$("confirm-cancel").click();
  assert.equal(page.slot(0, 2).classList.contains("sel"), false);
  page.control("discard 8").click();
  assert.equal(page.confirmText(), "discard the 8? peek own does not start on its own: press use peek own after, or end turn");
  page.ok();
  assert.deepEqual(page.socket.sent, [{ ref: 1, type: "discard", turn_seq: 5 }]);
});

test("a plain drawn card discards without a move", async (t) => {
  const page = await playing(t, { stage: { kind: "drawn", card: null } }, {}, { drawn: { card: 3 } });
  await page.settle(() => page.control("discard 3"));
  page.control("discard 3").click();
  assert.equal(page.confirmText(), "discard the 3?");
});

test("until the drawn card arrives the controls still work without it", async (t) => {
  const page = await playing(t, { stage: { kind: "drawn", card: null } }, {}, { drawn: [500, { message: "database error" }] });
  await page.settle(() => page.toast() === "database error");
  assert.equal(page.$("drawn"), null);
  page.slot(0, 1).click();
  assert.equal(page.confirmText(), "swap the drawn card into your card 2?");
  page.$("confirm-cancel").click();
  page.control("discard").click();
  assert.equal(page.confirmText(), "discard the drawn card?");
});

test("the drawn card is forgotten once the turn moves on", async (t) => {
  const page = await playing(t, { stage: { kind: "drawn", card: null } }, {}, { drawn: { card: 12 } });
  await page.settle(() => page.$("drawn"));
  page.push(makeView(makeGame({ turn: 1, turn_seq: 6 })));
  assert.equal(page.$("drawn"), null);
  // the next drawn card is a new reveal; the old value is not reused
  page.push(makeView(makeGame({ turn_seq: 9, stage: { kind: "drawn", card: null } })));
  assert.equal(page.$("drawn"), null);
  await page.settle(() => page.$("drawn"));
  assert.equal(page.reveals().length, 2);
});

test("a taken card must be swapped", async (t) => {
  const page = await playing(t, { stage: { kind: "taken", card: 4 } });
  assert.equal(page.$("prompt").textContent, "tap one of your cards to swap");
  assert.equal(page.control("discard 4"), undefined);
  page.slot(0, 0).click();
  page.ok();
  assert.deepEqual(page.socket.sent, [{ ref: 1, type: "swap", slot: 0, turn_seq: 5 }]);
});

test("peek own takes only your own card", async (t) => {
  const page = await playing(t, { stage: { kind: "special", mv: "peek_own" }, matchable: false });
  assert.equal(page.$("prompt").textContent, "tap one of your cards to peek, or skip");
  page.slot(1, 0).click();
  assert.equal(page.toast(), "that card can't be used for this move");
  assert.equal(page.confirmOpen(), false);
  page.slot(0, 1).click();
  assert.equal(page.confirmText(), "peek at your card 2?");
  page.ok();
  assert.deepEqual(page.socket.sent, [{ ref: 1, type: "peek", seat: 0, slot: 1, turn_seq: 5 }]);
});

test("peek other takes only another player's card", async (t) => {
  const page = await playing(t, { stage: { kind: "special", mv: "peek_other" }, matchable: false });
  assert.equal(page.$("prompt").textContent, "tap another player's card to peek, or skip");
  page.slot(0, 0).click();
  assert.equal(page.toast(), "that card can't be used for this move");
  page.slot(1, 3).click();
  assert.equal(page.confirmText(), "peek at bob's card 4?");
});

test("look and swap looks at any card, then swaps or keeps", async (t) => {
  const page = await playing(t, { stage: { kind: "special", mv: "look_swap" }, matchable: false });
  assert.equal(page.$("prompt").textContent, "tap any card to look at it, or skip");
  page.slot(0, 0).click();
  assert.equal(page.confirmText(), "look at your card 1?");
  page.$("confirm-cancel").click();
  page.slot(1, 2).click();
  assert.equal(page.confirmText(), "look at bob's card 3?");
  page.ok();

  page.push(makeView(makeGame({ stage: { kind: "looked", seat: 1, slot: 2 }, matchable: false, turn_seq: 6 })));
  assert.equal(page.$("prompt").textContent, "tap your card to swap with it, or keep yours");
  page.slot(1, 0).click();
  assert.equal(page.confirmOpen(), false);
  page.slot(0, 3).click();
  assert.equal(page.confirmText(), "swap your card 4 with bob's card 3?");
  page.ok();
  page.control("keep my cards").click();
  assert.equal(page.confirmText(), "keep your cards and end the turn?");
  page.ok();
  assert.deepEqual(page.socket.sent.slice(1), [
    { ref: 2, type: "look_swap", slot: 3, turn_seq: 6 },
    { ref: 3, type: "look_swap", slot: null, turn_seq: 6 },
  ]);
});

test("blind swap picks your card, then theirs", async (t) => {
  const page = await playing(t, { stage: { kind: "special", mv: "blind_swap" }, matchable: false });
  assert.equal(page.$("prompt").textContent, "tap your card, then another player's card, or skip");
  page.slot(1, 0).click();
  assert.equal(page.toast(), "that card can't be used for this move");
  page.slot(0, 0).click();
  assert.equal(page.confirmOpen(), false);
  assert.equal(page.$("prompt").textContent, "now tap another player's card");
  assert.ok(page.slot(0, 0).classList.contains("sel"));
  // a second own tap changes the pick
  page.slot(0, 1).click();
  assert.ok(page.slot(0, 1).classList.contains("sel"));
  assert.equal(page.slot(0, 0).classList.contains("sel"), false);
  page.slot(1, 2).click();
  assert.equal(page.confirmText(), "blind swap your card 2 with bob's card 3?");
  assert.ok(page.slot(0, 1).classList.contains("sel"));
  assert.ok(page.slot(1, 2).classList.contains("sel"));
  page.ok();
  assert.deepEqual(page.socket.sent, [{ ref: 1, type: "blind_swap", slot: 1, seat: 1, target_slot: 2, turn_seq: 5 }]);
});

test("a discarded special card waits: taps match, and the move is a button", async (t) => {
  const page = await playing(t, { stage: { kind: "earned", mv: "peek_other" }, discard_top: 9 });
  assert.equal(page.$("status").textContent, "your turn");
  // the prompt says the move has not started and what a tap does (UI-22)
  assert.equal(page.$("prompt").textContent,
    "peek other is earned but not started: press use peek other to start it, or end turn. tapping a card now is a match attempt on the 9");
  // no match mode to enter: a tap on a card is already a match
  assert.deepEqual(page.controls(), ["use peek other", "end turn", "KOMINO"]);
  assert.ok(page.control("use peek other").classList.contains("move-peek_other"));
  page.slot(1, 2).click();
  assert.equal(page.confirmText(),
    "match bob's card 3 with the 9? this is a match attempt, not peek other. if you're right, you then give them one of your cards.");
  page.ok();
  page.slot(0, 1).click();
  assert.equal(page.confirmText(), "match your card 2 with the 9? this is a match attempt, not peek other.");
  page.$("confirm-cancel").click();
  page.control("use peek other").click();
  assert.equal(page.confirmText(), "start peek other now? you then pick its card.");
  page.ok();
  page.control("end turn").click();
  assert.equal(page.confirmText(), "end your turn without using peek other?");
  page.ok();
  assert.deepEqual(page.socket.sent, [
    { ref: 1, type: "match", seq: 2, seat: 1, slot: 2, reaction_ms: 0 },
    { ref: 2, type: "use_special", turn_seq: 5 },
    { ref: 3, type: "skip", turn_seq: 5 },
  ]);
});

test("others see a waiting special move as the turn player's", async (t) => {
  const page = await playing(t, { turn: 1, stage: { kind: "earned", mv: "blind_swap" } });
  assert.equal(page.$("status").textContent, "bob's turn");
  assert.deepEqual(page.controls(), ["KOMINO"]);
});

test("a special move can be skipped", async (t) => {
  const page = await playing(t, { stage: { kind: "special", mv: "peek_own" }, matchable: false });
  page.control("skip move").click();
  assert.equal(page.confirmText(), "skip the special move?");
  page.ok();
  assert.deepEqual(page.socket.sent, [{ ref: 1, type: "skip", turn_seq: 5 }]);
});

test("ready confirms and carries no turn token", async (t) => {
  const page = await playing(t, { status: "peeking", ready_deadline: Date.now() + 20_500 });
  assert.equal(page.$("status").textContent, "memorize your bottom two cards. play starts in 21s or when everyone is ready.");
  assert.ok(page.doc.querySelector(".hand.mine .who").textContent.includes("(peeking)"));
  page.control("ready").click();
  assert.equal(page.confirmText(), "done memorizing your cards?");
  page.ok();
  assert.deepEqual(page.socket.sent, [{ ref: 1, type: "ready" }]);
  const g = makeGame({ status: "peeking", ready_deadline: Date.now() + 10_000 });
  g.seats[0].ready = true;
  page.push(makeView(g));
  assert.match(page.$("status").textContent, /^waiting for the others/);
  assert.equal(page.control("ready"), undefined);
  assert.ok(page.doc.querySelector(".hand.mine .who").textContent.includes("(ready)"));
});

test("the komino button explains when it can't be pressed", async (t) => {
  const cases = [
    [{ status: "peeking" }, "only during play"],
    [{ turn: 1 }, "only on your turn"],
    [{}, "everyone must take a turn first"],
  ];
  for (const [game, why] of cases) {
    const page = await playing(t, game);
    const b = page.control("KOMINO");
    assert.equal(b.disabled, true);
    assert.equal(b.title, why);
  }
  const page = await playing(t, { can_call: true });
  const b = page.control("KOMINO");
  assert.equal(b.disabled, false);
  assert.equal(b.title, "end the round");
  b.click();
  assert.equal(page.confirmText(), "call KOMINO? everyone else gets one more turn.");
  page.ok();
  assert.deepEqual(page.socket.sent, [{ ref: 1, type: "komino", turn_seq: 5 }]);
});

test("komino called mid turn says it lands when the turn ends", async (t) => {
  const page = await playing(t, { stage: { kind: "taken", card: 4 }, can_call: true });
  page.control("KOMINO").click();
  assert.equal(page.confirmText(), "call KOMINO? it takes effect when this turn ends, then everyone else gets one more turn.");
  page.ok();
  assert.deepEqual(page.socket.sent, [{ ref: 1, type: "komino", turn_seq: 5 }]);
  page.push(makeView(makeGame({ stage: { kind: "taken", card: 4 }, calling: true })));
  assert.equal(page.$("status").querySelector(".komino").textContent, "you called KOMINO; it takes effect when your turn ends. ");
  const b = page.control("KOMINO called");
  assert.equal(b.disabled, true);
  assert.equal(b.title, "called; it takes effect when your turn ends");
  // the turn goes on as usual
  page.slot(0, 1).click();
  assert.equal(page.confirmText(), "swap the 4 into your card 2?");
});

test("a confirm whose move went away closes and says why", async (t) => {
  const page = await playing(t);
  page.control("draw").click();
  page.push(makeView(makeGame({ turn: 1, turn_seq: 6 })));
  assert.equal(page.confirmOpen(), false);
  assert.equal(page.toast(), "that move is no longer available");
  // an unrelated update leaves an open confirm alone
  page.push(makeView(makeGame({ turn: 1, turn_seq: 6, deck_count: 40 })));
  page.slot(0, 0).click();
  assert.equal(page.confirmText(), "match your card 1 with the 4?");
  page.push(makeView(makeGame({ turn: 1, turn_seq: 6, deck_count: 30 })));
  assert.equal(page.confirmOpen(), true);
  page.push(makeView(makeGame({ turn: 1, turn_seq: 6, discard_seq: 3, discard_top: 9 })));
  assert.equal(page.confirmOpen(), false);
  assert.equal(page.toast(), "too late: the discard changed");
});

test("sending while disconnected asks to retry", async (t) => {
  const page = await boot();
  t.after(page.stop);
  page.control("draw").click();
  page.ok();
  assert.equal(page.toast(), "reconnecting, try again in a moment");
  assert.equal(page.socket.sent.length, 0);
});

test("forfeited, locked, and emptied seats render", async (t) => {
  const page = await playing(t, {
    turn: 1,
    caller: 1,
    final_remaining: [0],
    status: "final",
    seats: [seat("p0", ["x", null, "x", "x"]), seat("p1", ["x", "x", "x", "x"], { locked: true })],
  });
  assert.equal(page.doc.querySelectorAll(".card.empty").length, 1);
  assert.equal(page.doc.querySelectorAll(".lock").length, 4);
  assert.equal(page.$("status").querySelector(".komino").textContent, "KOMINO called by bob, 1 turn(s) left. ");
  assert.match(page.$("status").textContent, /bob's turn$/);
  page.push(makeView(makeGame({ seats: [seat("p0"), seat("p1", [], { forfeited: true })], caller: 0, final_remaining: [] })));
  assert.ok(page.doc.querySelectorAll(".hand .who")[0].textContent.includes("(out)"));
  assert.match(page.$("status").textContent, /^KOMINO called by you/);
});
