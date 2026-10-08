// Action effects: marks, flying cards, captions, sound cues (UI-23 - UI-28),
// and renders that keep buttons in place (UI-29).
import { test } from "node:test";
import assert from "node:assert/strict";
import { boot, ev, fakeAudio, makeGame, makeView, seat } from "../helpers.mjs";

const BOB_DREW = ev(3, "draw", "p1");

// bob's turn, with one event already seen
function bobsTurn(extra = {}, events = [BOB_DREW]) {
  return makeView(makeGame({ turn: 1, ...extra }), { events });
}

// boot with audio unlocked by a tap
async function withAudio(opts = {}) {
  const audio = fakeAudio();
  const page = await boot({ AudioContext: audio.Ctx, ...opts });
  page.doc.dispatchEvent(new page.w.Event("pointerdown"));
  return { page, audio };
}

const kinds = (audio) => audio.played.map((p) => p.kind);
const ghosts = (page) => page.doc.querySelectorAll("#fx .ghost");
const caption = (page) => (page.$("caption").hidden ? null : [...page.$("caption").children].map((d) => d.textContent));

test("the first view plays nothing; new events mark the cards they touched", async (t) => {
  const { page, audio } = await withAudio({ view: bobsTurn() });
  t.after(page.stop);
  assert.equal(caption(page), null);
  assert.equal(page.doc.querySelectorAll("#table [class*='fx-']").length, 0);
  assert.equal(ghosts(page).length, 0);
  assert.deepEqual(audio.played, []);

  const swap = ev(4, "blind_swap", "p1", { seat: 1, slot: 0, target_seat: 0, target_slot: 2 });
  page.push(bobsTurn({}, [swap, BOB_DREW]));
  assert.ok(page.slot(1, 0).classList.contains("fx-swap"));
  assert.ok(page.slot(0, 2).classList.contains("fx-swap"));
  assert.equal(ghosts(page).length, 2);
  assert.ok(ghosts(page)[0].style.getPropertyValue("--dx").endsWith("px"));
  assert.deepEqual(caption(page), ["bob blind swapped with your card 3"]);
  assert.ok(page.$("caption").firstElementChild.classList.contains("about-you"));
  assert.deepEqual(kinds(audio), ["noise", "noise"]);

  // the same events again replay nothing
  audio.clear();
  page.push(bobsTurn({}, [swap, BOB_DREW]));
  assert.equal(ghosts(page).length, 2);
  assert.deepEqual(audio.played, []);
});

test("marks survive re-renders and clear once they run out", async (t) => {
  const page = await boot({ view: bobsTurn() });
  t.after(page.stop);
  page.push(bobsTurn({}, [ev(4, "peek", "p1", { seat: 0, slot: 1 }), BOB_DREW]));
  const card = page.slot(0, 1);
  assert.ok(card.classList.contains("fx-peek"));
  assert.match(card.style.animationDelay, /^-\d+ms$/);
  page.push(bobsTurn({}, [ev(4, "peek", "p1", { seat: 0, slot: 1 }), BOB_DREW]));
  assert.equal(page.slot(0, 1), card, "an unchanged table keeps its elements");

  const now = Date.now;
  Date.now = () => now() + 2_000;
  try {
    page.push(bobsTurn({}, [ev(4, "peek", "p1", { seat: 0, slot: 1 }), BOB_DREW]));
  } finally {
    Date.now = now;
  }
  assert.equal(page.slot(0, 1).classList.contains("fx-peek"), false);
});

test("a shown miss turns its card face up for everyone, then back (SET-13)", async (t) => {
  const page = await boot({ view: bobsTurn() });
  t.after(page.stop);
  const miss = ev(4, "match", "p1", { ok: false, seat: 0, slot: 1, penalty: false, value: 9 });
  page.push(bobsTurn({}, [miss, BOB_DREW]));
  assert.ok(page.slot(0, 1).classList.contains("fx-miss"));
  assert.equal(page.label(0, 1), "your card 2: 9, peek other, missed match");
  assert.deepEqual(caption(page), ["bob missed a match on your card 2, a 9, and took a penalty"]);
  assert.equal(page.doc.querySelector("#log li").textContent, "bob missed a match on your card 2, a 9, and took a penalty");

  // it stays up past the usual mark, then flips back
  const now = Date.now;
  t.after(() => (Date.now = now));
  Date.now = () => now() + 2_000;
  page.push(bobsTurn({}, [miss, BOB_DREW]));
  assert.equal(page.label(0, 1), "your card 2: 9, peek other, missed match");
  Date.now = () => now() + 3_100;
  page.push(bobsTurn({}, [miss, BOB_DREW]));
  assert.equal(page.label(0, 1), "your card 2, face down");

  // a hidden miss names no value and leaves the card face down
  const hidden = ev(5, "match", "p1", { ok: false, seat: 0, slot: 2, penalty: false });
  page.push(bobsTurn({}, [hidden, miss, BOB_DREW]));
  assert.ok(page.slot(0, 2).classList.contains("fx-miss"));
  assert.equal(page.label(0, 2), "your card 3, face down");
  assert.deepEqual(caption(page), ["bob missed a match on your card 3 and took a penalty"]);
});

test("captions and flights fade on their own", async (t) => {
  const page = await boot({ view: bobsTurn() });
  t.after(page.stop);
  page.push(bobsTurn({}, [ev(4, "discard", "p1", { value: 5 }), BOB_DREW]));
  assert.deepEqual(caption(page), ["bob discarded 5"]);
  assert.equal(ghosts(page).length, 1);
  await new Promise((r) => setTimeout(r, 3_100));
  assert.equal(caption(page), null);
  assert.equal(ghosts(page).length, 0);
});

test("each event kind has its marks, flights, and sound", async (t) => {
  const { page, audio } = await withAudio({ view: bobsTurn() });
  t.after(page.stop);
  // each push lands after the last one's marks ran out
  const now = Date.now;
  let shift = 0;
  Date.now = () => now() + shift;
  t.after(() => (Date.now = now));
  let id = 3;
  // push one event by `who` on `game`, returning what it did
  const play = (kind, payload = {}, who = "p1", game = makeGame({ turn: 1 })) => {
    shift += 2_000;
    audio.clear();
    page.push(makeView(game, { events: [ev(++id, kind, who, payload)] }));
    const marked = [...page.doc.querySelectorAll("#table [class*='fx-']")].map((el) =>
      `${el.dataset.seat !== undefined ? `${el.dataset.seat}:${el.dataset.slot}` : el.id} ${[...el.classList].find((c) => c.startsWith("fx-"))}`);
    return { marked, sounds: audio.played.map((p) => (p.f ? `${p.kind} ${p.f}` : p.kind)), flights: ghosts(page).length };
  };
  const clearFx = () => page.doc.querySelectorAll("#fx .ghost").forEach((g) => g.remove());

  assert.deepEqual(play("draw"), { marked: ["deck fx-pulse"], sounds: ["noise"], flights: 1 });
  clearFx();
  assert.deepEqual(play("take", { value: 4 }), { marked: ["discard fx-pulse"], sounds: ["noise"], flights: 1 });
  clearFx();
  assert.deepEqual(play("swap", { seat: 1, slot: 3, discarded: 9 }), { marked: ["1:3 fx-flip"], sounds: ["noise"], flights: 1 });
  clearFx();
  assert.deepEqual(play("peek", { seat: 0, slot: 0 }), { marked: ["0:0 fx-peek"], sounds: ["noise", "sine 880"], flights: 0 });
  assert.deepEqual(play("look_swap", { seat: 1, slot: 0, target_seat: 0, target_slot: 0 }),
    { marked: ["1:0 fx-swap", "0:0 fx-swap"], sounds: ["noise", "noise"], flights: 2 });
  clearFx();
  // a hit on bob's own card, then on yours with a card given back
  assert.deepEqual(play("match", { ok: true, seat: 1, slot: 2, value: 4, give_slot: null }),
    { marked: ["discard fx-land"], sounds: ["sine 660", "sine 990"], flights: 1 });
  clearFx();
  const r = play("match", { ok: true, seat: 0, slot: 1, value: 4, give_slot: 3 });
  assert.deepEqual(r.marked.sort(), ["0:1 fx-flip", "discard fx-land"]);
  assert.equal(r.flights, 2);
  assert.ok(page.$("caption").firstElementChild.classList.contains("about-you"));
  clearFx();
  // a card given after the match lands face down in the emptied slot
  const g = play("give", { seat: 1, slot: 3, target_seat: 0, target_slot: 1 });
  assert.deepEqual(g, { marked: ["0:1 fx-flip"], sounds: ["noise"], flights: 1 });
  assert.ok(page.$("caption").firstElementChild.classList.contains("about-you"));
  clearFx();
  // a miss with a penalty card lands in the new last slot
  const five = makeGame({ turn: 1, seats: [seat("p0"), seat("p1", ["x", "x", "x", "x", "x"])] });
  assert.deepEqual(play("match", { ok: false, seat: 0, slot: 2, penalty: true }, "p1", five),
    { marked: ["1:4 fx-flip", "0:2 fx-miss"], sounds: ["sawtooth 180"], flights: 1 });
  clearFx();
  assert.deepEqual(play("match", { ok: false, seat: 1, slot: 0, penalty: false }),
    { marked: ["1:0 fx-miss"], sounds: ["sawtooth 180"], flights: 0 });
  assert.deepEqual(play("komino", { seat: 1 }).sounds, ["triangle 523", "triangle 659", "triangle 784", "triangle 1047"]);
  assert.ok(page.$("status").classList.contains("fx-flash"));
  const scored = play("scored", { winners: ["p0"] }, null, makeGame({ status: "scored", seats: [seat("p0", [1, 2, null, 3]), seat("p1", [4, 5, 6, 7])] }));
  assert.equal(scored.marked.length, 7);
  assert.ok(scored.marked.every((m) => m.endsWith("fx-flip")));
  assert.equal(scored.sounds.length, 6);
  assert.deepEqual(caption(page), ["game over: you"]);
  // a new game plays its first events
  assert.equal(play("start", {}, "p0", makeGame({ id: 2, turn: 1 })).sounds.length, 6);
  assert.deepEqual(play("forfeit", { seat: 1 }, "p1", makeGame({ id: 2, turn: 1 })), { marked: [], sounds: [], flights: 0 });
  assert.deepEqual(caption(page), ["bob left the game"]);
});

test("your own moves animate without a caption; ready says nothing", async (t) => {
  const page = await boot({ view: makeView(makeGame(), { events: [BOB_DREW] }) });
  t.after(page.stop);
  page.push(makeView(makeGame({ stage: { kind: "drawn" } }), { events: [ev(4, "draw", "p0"), BOB_DREW] }));
  assert.ok(page.$("deck").classList.contains("fx-pulse"));
  assert.equal(caption(page), null);
  page.push(makeView(makeGame({ stage: { kind: "drawn" } }), { events: [ev(5, "ready", "p1"), ev(4, "draw", "p0"), BOB_DREW] }));
  assert.equal(caption(page), null);
});

test("several events in one view play in order, captioned together", async (t) => {
  const { page, audio } = await withAudio({ view: bobsTurn() });
  t.after(page.stop);
  page.push(bobsTurn({}, [ev(5, "skip", "p1"), ev(4, "peek", "p1", { seat: 1, slot: 0 }), BOB_DREW]));
  assert.deepEqual(caption(page), ["bob peeked at bob's card 1", "bob skipped the move"]);
  assert.equal(page.$("caption").querySelector(".about-you"), null);
  assert.deepEqual(audio.played.map((p) => p.at), [10, 10.03]);
});

test("a flight from a player not at the table is skipped", async (t) => {
  const page = await boot({ view: bobsTurn() });
  t.after(page.stop);
  page.push(bobsTurn({}, [ev(4, "draw", "p9"), BOB_DREW]));
  assert.ok(page.$("deck").classList.contains("fx-pulse"));
  assert.equal(ghosts(page).length, 0);
});

test("a chime plays when your turn starts, once", async (t) => {
  const { page, audio } = await withAudio({ view: bobsTurn() });
  t.after(page.stop);
  page.push(makeView(makeGame({ turn: 0 }), { events: [BOB_DREW] }));
  assert.deepEqual(audio.played.map((p) => `${p.kind} ${p.f}`), ["sine 880", "sine 1320"]);
  audio.clear();
  page.push(makeView(makeGame({ turn: 0, stage: { kind: "drawn" } }), { events: [BOB_DREW] }));
  assert.deepEqual(audio.played, []);
  // play starting on your turn chimes too
  page.push(makeView(makeGame({ id: 3, status: "peeking" })));
  page.push(makeView(makeGame({ id: 3 })));
  assert.equal(audio.played.length, 2);
});

test("audio waits for a gesture and resumes a suspended context", async (t) => {
  const audio = fakeAudio();
  const page = await boot({ AudioContext: audio.Ctx, view: bobsTurn() });
  t.after(page.stop);
  page.push(bobsTurn({}, [ev(4, "draw", "p1"), BOB_DREW]));
  assert.equal(audio.contexts.length, 0);
  page.doc.dispatchEvent(new page.w.KeyboardEvent("keydown", { key: "a" }));
  page.doc.dispatchEvent(new page.w.Event("pointerdown"));
  assert.equal(audio.contexts.length, 1);
  assert.equal(audio.contexts[0].resumes, 1);
  page.push(bobsTurn({}, [ev(5, "draw", "p1"), BOB_DREW]));
  assert.deepEqual(kinds(audio), ["noise"]);
});

test("the sound button mutes cues and is remembered", async (t) => {
  const { page, audio } = await withAudio({ view: bobsTurn() });
  t.after(page.stop);
  assert.equal(page.$("sound").textContent, "sound on");
  assert.equal(page.$("sound").getAttribute("aria-pressed"), "true");
  page.$("sound").click();
  assert.equal(page.$("sound").textContent, "sound off");
  assert.equal(page.$("sound").getAttribute("aria-pressed"), "false");
  assert.equal(page.w.localStorage.getItem("komino.sound"), "off");
  page.push(bobsTurn({}, [ev(4, "draw", "p1"), BOB_DREW]));
  assert.deepEqual(audio.played, []);
  page.$("sound").click();
  assert.equal(page.w.localStorage.getItem("komino.sound"), "on");
  page.push(bobsTurn({}, [ev(5, "draw", "p1"), BOB_DREW]));
  assert.deepEqual(kinds(audio), ["noise"]);
});

test("a stored off starts muted; broken storage leaves sound on", async (t) => {
  const muted = await boot({ storage: { getItem: () => "off", setItem() {} } });
  t.after(muted.stop);
  assert.equal(muted.$("sound").textContent, "sound off");

  const broken = { getItem() { throw new Error("denied"); }, setItem() { throw new Error("denied"); } };
  const page = await boot({ storage: broken });
  t.after(page.stop);
  assert.equal(page.$("sound").textContent, "sound on");
  page.$("sound").click();
  assert.equal(page.$("sound").textContent, "sound off");
});

test("observers see every move animated and captioned", async (t) => {
  const watch = (events) => makeView(makeGame({ me: null, turn: 1 }), { me: null, observer: true, events });
  const page = await boot({ path: "/komino/r/ABCDEF/watch", view: watch([BOB_DREW]) });
  t.after(page.stop);
  page.push(watch([ev(4, "peek", "p1", { seat: 1, slot: 0 }), BOB_DREW]));
  assert.ok(page.slot(1, 0).classList.contains("fx-peek"));
  assert.deepEqual(caption(page), ["bob peeked at bob's card 1"]);
  assert.equal(page.$("caption").querySelector(".about-you"), null);
});

test("someone else's peek shows an eye on the card; your own does not", async (t) => {
  const page = await boot({ view: makeView(makeGame({ peeked: [{ seat: 0, slot: 1, until: null }] })) });
  t.after(page.stop);
  assert.ok(page.slot(0, 1).querySelector(".eye-badge"));
  assert.equal(page.label(0, 1), "your card 2, face down, being peeked at");
  page.push(makeView(makeGame({
    reveals: [{ id: 9, seat: 1, slot: 0, until: null }],
    peeked: [{ seat: 1, slot: 0, until: null }],
  })));
  assert.equal(page.slot(1, 0).querySelector(".eye-badge"), null);
});

test("buttons stay the same elements across renders, so a tap spanning one lands (UI-29)", async (t) => {
  const page = await boot();
  t.after(page.stop);
  const draw = page.control("draw");
  const deck = page.$("deck");
  const members = page.$("members").firstElementChild;
  // a new view with the same table and controls, as each countdown tick renders
  const next = () => makeView(makeGame({ turn_seq: 6 }));
  page.push(next());
  assert.equal(page.control("draw"), draw);
  assert.equal(page.$("deck"), deck);
  assert.equal(page.$("members").firstElementChild, members);
  // the kept button and pile act on the latest turn: a choice made against
  // the old one would close at the next view
  for (const el of [draw, deck]) {
    el.click();
    page.push(next());
    assert.equal(page.confirmOpen(), true);
    page.ok();
    assert.deepEqual(page.live().sent.at(-1), { ref: page.live().sent.length, type: "draw", turn_seq: 6 });
  }
  page.push(makeView(makeGame({ turn_seq: 7, discard_top: 2 })));
  page.$("discard").click();
  assert.equal(page.confirmText(), "take the 2 from the discard pile?");
});
