// Play aids: fast match (UI-40), background alerts (UI-35), first game tips
// (UI-37), memory marks (SET-17), keyboard shortcuts (UI-39), and screen
// reader announcements (UI-42).
import { test } from "node:test";
import assert from "node:assert/strict";
import { boot, ev, fakeNotifications, makeGame, makeView, seat } from "../helpers.mjs";

/** A localStorage stand-in that two pages can share, as one browser does. */
function memoryStorage(init = {}) {
  const m = new Map(Object.entries(init));
  return {
    getItem: (k) => (m.has(k) ? m.get(k) : null),
    setItem: (k, v) => m.set(k, String(v)),
    map: m,
  };
}
const broken = {
  getItem() {
    throw new Error("denied");
  },
  setItem() {
    throw new Error("denied");
  },
};
const noTips = () => memoryStorage({ "komino.tips": "off" });
const roomWith = (settings) => ({
  code: "ABCDEF",
  host: "p0",
  url: "https://kominick.com/komino/r/ABCDEF",
  watch_url: "https://kominick.com/komino/r/ABCDEF/watch",
  settings,
});
const ICON = 'data:image/svg+xml,%3Csvg%20xmlns%3D%22http%3A%2F%2Fwww.w3.org%2F2000%2Fsvg%22%20viewBox%3D%220%200%2032%2032%22%3E%3Crect%20x%3D%225%22%20y%3D%222%22%20width%3D%2222%22%20height%3D%2228%22%20rx%3D%224%22%20fill%3D%22%2329466b%22%2F%3E%3Cpath%20d%3D%22M16%208%20L22%2016%20L16%2024%20L10%2016%20Z%22%20fill%3D%22%233c5f8c%22%2F%3E%3C%2Fsvg%3E';

// ---------------------------------------------------------------- fast match

test("fast match sends a match on the first tap and is remembered (UI-40)", async (t) => {
  const storage = noTips();
  const page = await boot({ view: makeView(makeGame({ turn: 1 })), storage });
  t.after(page.stop);
  const s = page.live();
  assert.equal(page.$("fast").textContent, "fast match off");
  page.$("fast").click();
  assert.equal(page.$("fast").textContent, "fast match on");
  assert.equal(page.$("fast").getAttribute("aria-pressed"), "true");
  assert.equal(storage.map.get("komino.fast"), "on");
  assert.match(page.toast(), /^fast match on/);
  page.slot(1, 2).click();
  assert.equal(page.confirmOpen(), false);
  assert.deepEqual({ ...s.sent.at(-1), ref: 0 }, { ref: 0, type: "match", seq: 2, seat: 1, slot: 2, reaction_ms: 0 });
  assert.ok(page.slot(1, 2).classList.contains("claiming"));

  // a second page in the same browser starts with it on
  const again = await boot({ view: makeView(makeGame({ turn: 1 })), storage });
  t.after(again.stop);
  assert.equal(again.$("fast").textContent, "fast match on");
  again.live();
  again.slot(0, 0).click();
  assert.equal(again.socket.sent.at(-1).type, "match");
  again.$("fast").click();
  assert.equal(storage.map.get("komino.fast"), "off");
  assert.match(again.toast(), /^fast match off/);
});

test("fast match still asks before giving a card, and storage that throws leaves it off", async (t) => {
  const owed = [{ from: 0, seat: 1, slot: 0, deadline: Date.now() + 9_000 }];
  const page = await boot({ view: makeView(makeGame({ turn: 1, owed })), storage: broken });
  t.after(page.stop);
  page.live();
  assert.equal(page.$("fast").textContent, "fast match off");
  page.$("fast").click();
  assert.equal(page.$("fast").textContent, "fast match on");
  page.slot(0, 1).click();
  assert.equal(page.confirmText(), "give your card 2 to bob's card 1?");
});

// ---------------------------------------------------------------- alerts

test("a hidden page shows whose turn it is in the title and icon, and notifies when allowed (UI-35)", async (t) => {
  const n = fakeNotifications("granted");
  const page = await boot({ view: makeView(makeGame({ turn: 1 })), storage: noTips(), Notification: n.N });
  t.after(page.stop);
  page.live();
  assert.equal(page.doc.title, "komino");
  assert.equal(page.$("favicon").getAttribute("href"), ICON);
  page.$("alerts").click();
  assert.equal(page.$("alerts").textContent, "alerts on");
  // permission was already granted, so nothing is asked
  assert.equal(n.requests.length, 0);

  page.setHidden(true);
  page.push(makeView(makeGame({ turn: 0, turn_seq: 6 })));
  assert.equal(page.doc.title, "(!) your turn - komino");
  assert.notEqual(page.$("favicon").getAttribute("href"), ICON);
  assert.deepEqual(n.shown, [{ title: "komino", body: "your turn" }]);
  assert.equal(page.vibrations.length, 1);

  page.setHidden(false);
  assert.equal(page.doc.title, "komino");
  assert.equal(page.$("favicon").getAttribute("href"), ICON);
});

test("alerts ask for permission once turned on, and stay quiet on a visible page", async (t) => {
  const n = fakeNotifications("default");
  const page = await boot({ view: makeView(makeGame({ turn: 1 })), storage: noTips(), Notification: n.N });
  t.after(page.stop);
  page.live();
  page.$("alerts").click();
  assert.equal(n.requests.length, 1);
  page.$("alerts").click();
  assert.equal(page.$("alerts").textContent, "alerts off");
  // visible: a vibration but no title change or notification
  page.push(makeView(makeGame({ turn: 0, turn_seq: 6 })));
  assert.equal(page.doc.title, "komino");
  assert.equal(n.shown.length, 0);
  assert.equal(page.vibrations.length, 1);
  // showing an unalerted page changes nothing
  page.setHidden(false);
  assert.equal(page.doc.title, "komino");
});

test("an owed card and a new game also alert a hidden page; a refused notification is ignored", async (t) => {
  class Refusing {
    constructor() {
      throw new Error("use a service worker");
    }
  }
  Refusing.permission = "granted";
  const page = await boot({ view: makeView(makeGame({ turn: 1 })), storage: memoryStorage({ "komino.alerts": "on", "komino.tips": "off" }), Notification: Refusing });
  t.after(page.stop);
  page.live();
  assert.equal(page.$("alerts").textContent, "alerts on");
  page.setHidden(true);
  page.push(makeView(makeGame({ turn: 1, owed: [{ from: 0, seat: 1, slot: 0, deadline: Date.now() + 9_000 }] })));
  assert.equal(page.doc.title, "(!) give a card for your match - komino");
  page.setHidden(false);
  page.setHidden(true);
  page.push(makeView(makeGame({ id: 2, turn: 1, status: "peeking", ready_deadline: Date.now() + 9_000 })));
  assert.equal(page.doc.title, "(!) a new game started - komino");
});

// ---------------------------------------------------------------- tips

test("first game tips show once each, and can be turned off (UI-37)", async (t) => {
  const storage = memoryStorage();
  const g = makeGame({ status: "peeking", ready_deadline: Date.now() + 9_000 });
  const page = await boot({ view: makeView(g), storage });
  t.after(page.stop);
  page.live();
  assert.equal(page.$("tip").hidden, false);
  assert.match(page.$("tip-text").textContent, /^tip: these face up cards are yours/);
  page.$("tip-ok").click();
  assert.equal(page.$("tip").hidden, true);
  assert.equal(storage.map.get("komino.tips"), '["peek"]');

  page.push(makeView(makeGame()));
  assert.match(page.$("tip-text").textContent, /^tip: on your turn, draw/);
  page.$("tip-ok").click();
  page.push(makeView(makeGame({ stage: { kind: "drawn", card: null }, turn_seq: 6 })));
  assert.match(page.$("tip-text").textContent, /^tip: only you see the drawn card/);
  page.$("tip-ok").click();
  page.push(makeView(makeGame({ stage: { kind: "earned", mv: "peek_own" }, turn_seq: 7, can_call: true })));
  assert.match(page.$("tip-text").textContent, /^tip: you discarded a special card/);
  page.$("tip-ok").click();
  assert.match(page.$("tip-text").textContent, /^tip: when you think your total is the lowest/);
  page.$("tip-ok").click();
  page.push(makeView(makeGame({ turn: 1 })));
  assert.match(page.$("tip-text").textContent, /^tip: when the discard pile says matchable/);
  page.push(makeView(makeGame({ turn: 1, owed: [{ from: 0, seat: 1, slot: 0, deadline: Date.now() + 9_000 }] })));
  assert.match(page.$("tip-text").textContent, /^tip: you matched another player's card/);
  page.$("tip-off").click();
  assert.equal(page.$("tip").hidden, true);
  assert.equal(storage.map.get("komino.tips"), "off");

  // a later page in the same browser shows no tips; a broken list starts over
  const later = await boot({ view: makeView(makeGame()), storage });
  t.after(later.stop);
  assert.equal(later.$("tip").hidden, true);
  const fresh = await boot({ view: makeView(makeGame()), storage: memoryStorage({ "komino.tips": "{" }) });
  t.after(fresh.stop);
  assert.equal(fresh.$("tip").hidden, false);
});

// ---------------------------------------------------------------- marks

const marked = (extra = {}, game = makeGame({ turn: 1, matchable: false })) =>
  makeView(game, { room: roomWith({ hand_size: 4, away_grace_secs: 30, turn_limit_secs: null, reveal_secs: 15, show_misses: true, memory_marks: true }), ...extra });

function rightClick(page, el) {
  const e = new page.w.MouseEvent("contextmenu", { bubbles: true, cancelable: true });
  el.dispatchEvent(e);
  return e;
}

test("a right click marks a face down card, seen only here (SET-17)", async (t) => {
  const storage = noTips();
  const page = await boot({ view: marked(), storage });
  t.after(page.stop);
  page.live();
  const e = rightClick(page, page.slot(1, 2));
  assert.ok(e.defaultPrevented);
  assert.equal(page.$("marks").hidden, false);
  assert.equal(page.$("marks-title").textContent, "mark bob's card 3");
  const values = [...page.$("marks-values").querySelectorAll("button")].map((b) => b.textContent);
  assert.equal(values.length, 15);
  assert.equal(values[0], "-1");
  page.$("marks-values").querySelector('[data-v="7"]').click();
  assert.equal(page.$("marks").hidden, true);
  assert.equal(page.slot(1, 2).querySelector(".mark").textContent, "7?");
  assert.equal(page.label(1, 2), "bob's card 3, face down, marked 7");
  assert.deepEqual(JSON.parse(storage.map.get("komino.marks.ABCDEF")), { game: 1, at: { "1:2": 7 } });

  // the picker shows the current mark; clear removes it
  rightClick(page, page.slot(1, 2));
  assert.ok(page.$("marks-values").querySelector('[data-v="7"]').classList.contains("primary"));
  page.$("marks-clear").click();
  assert.equal(page.slot(1, 2).querySelector(".mark"), null);

  // cancel, escape, and the backdrop close it
  rightClick(page, page.slot(0, 0));
  assert.equal(page.$("marks-title").textContent, "mark your card 1");
  page.$("marks-cancel").click();
  assert.equal(page.$("marks").hidden, true);
  rightClick(page, page.slot(0, 0));
  page.key("Escape");
  assert.equal(page.$("marks").hidden, true);
  rightClick(page, page.slot(0, 0));
  page.$("marks").dispatchEvent(new page.w.MouseEvent("click", { bubbles: true }));
  assert.equal(page.$("marks").hidden, true);
  assert.equal(page.socket.sent.length, 0);
});

test("a long press marks a card without tapping it", async (t) => {
  const page = await boot({ view: marked({}, makeGame({ turn: 1 })), storage: noTips() });
  t.after(page.stop);
  page.live();
  const card = page.slot(1, 0);
  card.dispatchEvent(new page.w.Event("pointerdown"));
  await new Promise((r) => setTimeout(r, 500));
  assert.equal(page.$("marks").hidden, false);
  page.$("marks-values").querySelector('[data-v="3"]').click();
  // the click that ends the press is not a match
  page.slot(1, 0).click();
  assert.equal(page.confirmOpen(), false);
  // a short press is a tap
  const again = page.slot(1, 1);
  again.dispatchEvent(new page.w.Event("pointerdown"));
  again.dispatchEvent(new page.w.Event("pointerup"));
  await new Promise((r) => setTimeout(r, 500));
  assert.equal(page.$("marks").hidden, true);
  again.click();
  assert.equal(page.confirmOpen(), true);
});

test("marks follow cards as they move and reset with a new game", async (t) => {
  const storage = memoryStorage({
    "komino.tips": "off",
    "komino.marks.ABCDEF": JSON.stringify({ game: 1, at: { "0:0": 1, "0:1": 2, "1:0": 5, "1:1": 6, "1:3": 9 } }),
  });
  const page = await boot({ view: marked(), storage });
  t.after(page.stop);
  const mark = (s, n) => {
    const m = page.doc.querySelector(`[data-seat="${s}"][data-slot="${n}"] .mark`);
    return m ? m.textContent : null;
  };
  assert.equal(mark(1, 0), "5?");
  const events = [];
  const push = (e, game = makeGame({ turn: 1, matchable: false })) => {
    events.unshift(e);
    page.push(marked({ events: [...events] }, game));
  };
  push(ev(1, "draw", "p1"));
  // bob's blind swap trades the marks of the two cards
  push(ev(2, "blind_swap", "p1", { seat: 1, slot: 0, target_seat: 0, target_slot: 0 }));
  assert.deepEqual([mark(0, 0), mark(1, 0)], ["5?", "1?"]);
  // a swap clears the slot it replaced
  push(ev(3, "swap", "p1", { seat: 1, slot: 1, discarded: 4 }));
  assert.equal(mark(1, 1), null);
  // a right match clears the matched card; a given card carries its mark
  push(ev(4, "match", "p0", { ok: true, seat: 1, slot: 3, value: 9, owes: true }),
    makeGame({ turn: 1, seats: [seat("p0"), seat("p1", ["x", "x", "x", null])] }));
  push(ev(5, "give", "p0", { seat: 0, slot: 1, target_seat: 1, target_slot: 3 }),
    makeGame({ turn: 1, seats: [seat("p0", ["x", null, "x", "x"]), seat("p1")] }));
  assert.equal(mark(1, 3), "2?");
  // a match given for up front moves the giver's mark too
  push(ev(6, "match", "p0", { ok: true, seat: 1, slot: 2, value: 4, give_slot: 0 }),
    makeGame({ turn: 1, seats: [seat("p0", [null, null, "x", "x"]), seat("p1")] }));
  assert.equal(mark(1, 2), "5?");
  // look and swap trades two unmarked and marked cards; a miss moves nothing
  push(ev(7, "look_swap", "p1", { seat: 1, slot: 2, target_seat: 0, target_slot: 3 }));
  assert.deepEqual([mark(1, 2), mark(0, 3)], [null, "5?"]);
  push(ev(8, "match", "p1", { ok: false, seat: 0, slot: 3, penalty: true }));
  assert.equal(mark(0, 3), "5?");

  // a new game starts unmarked
  page.push(marked({ events: [] }, makeGame({ id: 2, turn: 1, matchable: false })));
  assert.equal(page.doc.querySelectorAll(".mark").length, 0);
  assert.deepEqual(JSON.parse(storage.map.get("komino.marks.ABCDEF")), { game: 1, at: { "1:0": 1, "1:3": 2, "0:3": 5 } });
});

test("marks need the room to allow them; a bad saved value starts unmarked", async (t) => {
  const page = await boot({ view: makeView(makeGame({ turn: 1, matchable: false })), storage: memoryStorage({ "komino.marks.ABCDEF": "{" }) });
  t.after(page.stop);
  const e = rightClick(page, page.slot(1, 0));
  assert.equal(e.defaultPrevented, false);
  page.slot(1, 0).dispatchEvent(new page.w.Event("pointerdown"));
  await new Promise((r) => setTimeout(r, 500));
  assert.equal(page.$("marks").hidden, true);

  const allowed = await boot({ view: marked(), storage: memoryStorage({ "komino.marks.ABCDEF": "{" }) });
  t.after(allowed.stop);
  assert.equal(allowed.doc.querySelectorAll(".mark").length, 0);
});

// ---------------------------------------------------------------- keys

test("letters press the controls and digits tap your cards (UI-39)", async (t) => {
  const page = await boot({ storage: noTips() });
  t.after(page.stop);
  page.live();
  const e = page.key("d");
  assert.ok(e.defaultPrevented);
  assert.equal(page.confirmText(), "draw from the deck?");
  // with a dialog open, keys do nothing but Enter and Escape
  page.key("t");
  assert.equal(page.confirmText(), "draw from the deck?");
  page.key("Escape");
  page.key("T");
  assert.equal(page.confirmText(), "take the 4 from the discard pile?");
  page.key("Escape");
  // a key with no control, or a disabled one, does nothing
  page.key("u");
  page.key("k");
  assert.equal(page.confirmOpen(), false);

  page.push(makeView(makeGame({ stage: { kind: "taken", card: 4 }, turn_seq: 6 })));
  page.key("2");
  assert.equal(page.confirmText(), "swap the 4 into your card 2?");
  page.key("Escape");
  // no card 10 here
  page.key("0");
  assert.equal(page.confirmOpen(), false);
  // typing a name or holding a modifier is not a shortcut
  page.key("d", page.$("name"));
  page.doc.body.dispatchEvent(new page.w.KeyboardEvent("keydown", { key: "1", ctrlKey: true, bubbles: true }));
  assert.equal(page.confirmOpen(), false);
  page.key("?");
  assert.equal(page.$("guide").hidden, false);
  page.key("?");
  page.key("1");
  assert.equal(page.confirmOpen(), false);
});

test("keys reach every kind of control", async (t) => {
  const page = await boot({ storage: noTips() });
  t.after(page.stop);
  page.live();
  page.push(makeView(makeGame({ stage: { kind: "drawn", card: null }, turn_seq: 6 })));
  page.key("x");
  assert.equal(page.confirmText(), "discard the drawn card?");
  page.key("Escape");
  page.key("m");
  assert.match(page.toast(), /^tap a card you think is a 4/);
  page.push(makeView(makeGame({ stage: { kind: "earned", mv: "peek_own" }, turn_seq: 7 })));
  page.key("u");
  assert.match(page.confirmText(), /^start peek own now/);
  page.key("Escape");
  page.key("e");
  assert.equal(page.confirmText(), "end your turn without using peek own?");
  page.key("Escape");
  page.push(makeView(makeGame({ status: "peeking", ready_deadline: Date.now() + 9_000 })));
  page.key("r");
  assert.equal(page.confirmText(), "done memorizing your cards?");
  page.key("Escape");
  const g = makeGame({ turn: 1, can_call: false, reveals: [{ id: "r1", seat: 1, slot: 0, until: null }] });
  page.push(makeView(g));
  page.key("h");
  assert.equal(page.socket.sent.at(-1).type, "hide");
  page.push(makeView(makeGame({ can_call: true, turn_seq: 8 })));
  page.key("k");
  assert.match(page.confirmText(), /^call KOMINO/);

  // an observer's keys do nothing
  const watch = await boot({ path: "/komino/r/ABCDEF/watch", view: makeView(makeGame({ me: null }), { me: null, observer: true }) });
  t.after(watch.stop);
  watch.key("1");
  watch.key("d");
  assert.equal(watch.confirmOpen(), false);
  // nor do they in the lobby, beyond the guide
  const lobby = await boot({ path: "/komino" });
  t.after(lobby.stop);
  lobby.key("d");
  lobby.key("?");
  assert.equal(lobby.$("guide").hidden, false);
});

// ---------------------------------------------------------------- announcements

test("screen readers hear each new event and the start of your turn (UI-42)", async (t) => {
  const page = await boot({ view: makeView(makeGame({ turn: 1 })), storage: noTips() });
  t.after(page.stop);
  assert.equal(page.$("announce").getAttribute("aria-live"), "polite");
  const events = [ev(1, "draw", "p1")];
  const push = (game, ...fresh) => {
    events.unshift(...fresh);
    page.push(makeView(game, { events: [...events] }));
  };
  push(makeGame({ turn: 1 }));
  assert.equal(page.$("announce").textContent, "bob drew a card");
  push(makeGame({ turn: 1 }), ev(3, "ready", "p1"), ev(2, "discard", "p1", { value: 3 }));
  assert.equal(page.$("announce").textContent, "bob discarded 3");
  push(makeGame({ turn: 0, turn_seq: 6 }), ev(4, "skip", "p1"));
  assert.equal(page.$("announce").textContent, "your turn");
  push(makeGame({ turn: 0, turn_seq: 7 }), ev(5, "draw", "p0"));
  assert.equal(page.$("announce").textContent, "you drew a card");
  // the same words again are still read
  push(makeGame({ turn: 0, turn_seq: 7 }), ev(6, "draw", "p0"));
  assert.equal(page.$("announce").textContent, "you drew a card.");
});

test("your own missed match buzzes the phone", async (t) => {
  const page = await boot({ view: makeView(makeGame({ turn: 1 })), storage: noTips() });
  t.after(page.stop);
  page.push(makeView(makeGame({ turn: 1 }), { events: [ev(1, "match", "p0", { ok: false, seat: 1, slot: 0, penalty: true })] }));
  assert.deepEqual(page.vibrations, [200]);
  page.push(makeView(makeGame({ turn: 1 }), { events: [ev(2, "match", "p1", { ok: false, seat: 0, slot: 0, penalty: true }),
    ev(1, "match", "p0", { ok: false, seat: 1, slot: 0, penalty: true })] }));
  assert.deepEqual(page.vibrations, [200]);
});
