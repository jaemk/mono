// The rules guide (UI-34): a modal from the header that describes the game
// as the room's settings play it.
import { test } from "node:test";
import assert from "node:assert/strict";
import { boot, makeGame, makeView } from "../helpers.mjs";

const room = (settings) => ({
  code: "ABCDEF",
  host: "p0",
  url: "https://kominick.com/komino/r/ABCDEF",
  watch_url: "https://kominick.com/komino/r/ABCDEF/watch",
  settings,
});

async function inRoom(t, settings) {
  const page = await boot({ view: makeView(makeGame(), { room: room(settings) }) });
  t.after(page.stop);
  page.live();
  return page;
}

const guideText = (page) => page.$("guide-body").textContent.replace(/\s+/g, " ");

test("the rules button opens the guide with this room's settings", async (t) => {
  const page = await inRoom(t, { hand_size: 6, turn_limit_secs: 60, away_grace_secs: 120, reveal_secs: null, show_misses: false });
  assert.equal(page.$("guide").hidden, true);
  page.$("guide-btn").click();
  assert.equal(page.$("guide").hidden, false);
  assert.equal(page.doc.activeElement, page.$("guide-close"));
  assert.equal(page.$("guide-room").textContent, "as this room plays");
  const text = guideText(page);
  for (const want of [
    "dealt 6 cards face down",
    "near row (cards 1 to 3)",
    "Each turn must end within 60 seconds or it is skipped.",
    "it is skipped after 120 seconds",
    "stays face up until you press hide card",
    "Nobody learns that card's value, you included.",
    "You have 15 seconds, then your highest card is given for you.",
    "The move does not start on its own.",
  ]) assert.ok(text.includes(want), `missing: ${want}`);
});

test("the guide follows the defaults when the room kept them", async (t) => {
  const page = await inRoom(t, { hand_size: 4, turn_limit_secs: null, away_grace_secs: 30, reveal_secs: 15, show_misses: true });
  page.$("guide-btn").click();
  const text = guideText(page);
  for (const want of ["dealt 4 cards", "cards 1 to 2", "Turns have no time limit.", "for 15 seconds, or until you press hide card",
    "Everyone sees that card's value."]) assert.ok(text.includes(want), `missing: ${want}`);
});

test("the guide explains match play, marks, and the keys as the room sets them", async (t) => {
  const page = await inRoom(t, { hand_size: 4, turn_limit_secs: null, away_grace_secs: 30, reveal_secs: 15, show_misses: true,
    target_score: 100, caller_penalty: 10, exact_reset: true, memory_marks: true });
  page.$("guide-btn").click();
  const text = guideText(page);
  for (const want of [
    "Once a total reaches 100, the lowest total wins the match",
    "A caller who does not win adds 10 points to their score.",
    "A total that lands exactly on 100 is halved.",
    "Press and hold a card (or right click it)",
    "d draw, t take, x discard",
  ]) assert.ok(text.includes(want), `missing: ${want}`);

  page.$("guide-close").click();
  page.push(makeView(makeGame(), { room: room({ hand_size: 4, turn_limit_secs: null, away_grace_secs: 30, reveal_secs: 15,
    show_misses: true, target_score: null, caller_penalty: 0, exact_reset: true, memory_marks: false }) }));
  page.$("guide-btn").click();
  const plain = guideText(page);
  assert.ok(plain.includes("Scores add up to running totals from game to game, with no target."));
  assert.ok(plain.includes("This room does not allow memory marks."));
  assert.ok(!plain.includes("is halved"));
});

test("every special card is shown with its move", async (t) => {
  const page = await inRoom(t, { hand_size: 4, turn_limit_secs: null, away_grace_secs: 30, reveal_secs: 15, show_misses: true });
  page.$("guide-btn").click();
  const moves = [...page.doc.querySelectorAll("#guide-body .guide-moves li")];
  assert.deepEqual(moves.map((li) => li.querySelector("strong").textContent),
    ["7, 8: peek own", "9, 10: peek other", "11, 12: blind swap", "13: look and swap"]);
  assert.deepEqual(moves.map((li) => li.querySelectorAll("svg").length), [2, 2, 2, 1]);
});

test("close, escape, and the backdrop close the guide; escape leaves an open confirm alone", async (t) => {
  const page = await inRoom(t, null);
  const escape = () => page.doc.dispatchEvent(new page.w.KeyboardEvent("keydown", { key: "Escape" }));
  page.$("guide-btn").click();
  page.$("guide-close").click();
  assert.equal(page.$("guide").hidden, true);
  page.$("guide-btn").click();
  page.$("guide-body").dispatchEvent(new page.w.MouseEvent("click", { bubbles: true }));
  assert.equal(page.$("guide").hidden, false);
  page.$("guide").dispatchEvent(new page.w.MouseEvent("click", { bubbles: true }));
  assert.equal(page.$("guide").hidden, true);
  page.control("draw").click();
  page.$("guide-btn").click();
  escape();
  assert.equal(page.$("guide").hidden, true);
  assert.equal(page.confirmOpen(), true);
  escape();
  assert.equal(page.confirmOpen(), false);
});

test("in the lobby the guide names the settings as room choices", async (t) => {
  const page = await boot({ path: "/komino" });
  t.after(page.stop);
  page.$("guide-btn").click();
  assert.equal(page.$("guide-room").textContent, "rooms choose the hand size, timers, and peek time");
  const text = guideText(page);
  for (const want of ["the room's hand size of cards (4 to 10)", "A room can set a turn limit",
    "for the room's peek time", "A room can also show everyone the card's value.", "A room can play to a target",
    "A room can let players mark cards"]) assert.ok(text.includes(want), `missing: ${want}`);
});
