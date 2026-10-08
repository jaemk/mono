// The table's look: seat colors (UI-36), seats around the table, earlier
// discards (RULE-28), the discard glow (UI-41), the end of game summary
// (UI-38), and reaction times (UI-43).
import { test } from "node:test";
import assert from "node:assert/strict";
import { boot, ev, makeGame, makeView, member, seat } from "../helpers.mjs";

const COLORS = ["#e8a23a", "#4fb3e8", "#e0607e", "#7fd16b", "#b48cf2", "#f07d4a", "#46c9b4", "#d8d05a"];
const color = (el) => el.style.getPropertyValue("--seat").trim();
const settings = (extra = {}) => ({ hand_size: 4, away_grace_secs: 30, turn_limit_secs: null, reveal_secs: 15, show_misses: true, ...extra });
const room = (extra, history = []) => ({
  code: "ABCDEF",
  host: "p0",
  url: "https://kominick.com/komino/r/ABCDEF",
  watch_url: "https://kominick.com/komino/r/ABCDEF/watch",
  settings: settings(extra),
  history,
});

const names = ["ada", "bob", "cy", "dee", "eve"];
function five(extra = {}) {
  return makeView(makeGame({ seats: names.map((_, i) => seat(`p${i}`)), ...extra }), {
    members: names.map((n, i) => member(`p${i}`, n)),
  });
}

test("each seat has its color on its hand, log lines, and member row (UI-36)", async (t) => {
  const view = five();
  view.events = [ev(2, "discard", "p2", { value: 3 }), ev(1, "start", "p0")];
  const page = await boot({ view });
  t.after(page.stop);
  for (let i = 0; i < 5; i++) assert.equal(color(page.doc.querySelector(`.hand[data-hand="${i}"]`)), COLORS[i]);
  const lines = [...page.$("log").querySelectorAll("li")];
  assert.deepEqual(lines.map(color), [COLORS[2], COLORS[0]]);
  assert.ok(lines[0].classList.contains("by"));
  const chips = [...page.$("members").querySelectorAll(".chip")];
  assert.deepEqual(chips.map(color), COLORS.slice(0, 5));

  // a caption is colored by who acted
  const v = five({ turn: 1 });
  v.events = [ev(3, "draw", "p3"), ...view.events];
  page.push(v);
  assert.equal(color(page.$("caption").querySelector("div")), COLORS[3]);
});

test("other players sit to your left, across, and right, clockwise", async (t) => {
  const side = (page, cls) => [...page.doc.querySelectorAll(`.side.${cls} .hand`)].map((h) => Number(h.dataset.hand));
  // you are seat 2 of 5: seat 3 sits on your left, 4 and 0 across, 1 on your right
  const view = five({ me: 2 });
  view.me = "p2";
  const page = await boot({ view, me: { id: "p2", name: "cy" } });
  t.after(page.stop);
  assert.deepEqual(side(page, "left"), [3]);
  assert.deepEqual(side(page, "top"), [4, 0]);
  assert.deepEqual(side(page, "right"), [1]);
  assert.equal(page.doc.querySelector(".table > .hand.mine").dataset.hand, "2");

  // with one or two others everyone sits across
  const two = await boot();
  t.after(two.stop);
  assert.deepEqual(side(two, "top"), [1]);
  assert.deepEqual(side(two, "left"), []);

  // the watch page seats everyone, from seat 0
  const watch = await boot({ path: "/komino/r/ABCDEF/watch", view: five({ me: null }) });
  t.after(watch.stop);
  assert.deepEqual([...side(watch, "left"), ...side(watch, "top"), ...side(watch, "right")], [0, 1, 2, 3, 4]);
});

test("the discards before the top one are shown, newest first (RULE-28)", async (t) => {
  const page = await boot({ view: makeView(makeGame({ discard_recent: [4, 9, 2, 13] })) });
  t.after(page.stop);
  const recent = page.doc.querySelector(".recent");
  assert.equal(recent.getAttribute("aria-label"), "earlier discards, newest first: 9, 2, 13");
  assert.equal(recent.querySelectorAll(".card.mini svg").length, 3);
  // each shows its value, a special card's too, not just its move (UI-44)
  assert.deepEqual([...recent.querySelectorAll(".card.mini svg text")].map((n) => n.textContent), ["9", "2", "13"]);
  page.push(makeView(makeGame({ discard_recent: [4] })));
  assert.equal(page.doc.querySelector(".recent"), null);
  page.push(makeView(makeGame()));
  assert.equal(page.doc.querySelector(".recent"), null);
});

test("a matchable discard glows, picking up where it was on a re-render (UI-41)", async (t) => {
  t.mock.timers.enable({ apis: ["Date"], now: 5_000_000 });
  const page = await boot();
  t.after(page.stop);
  const discard = page.$("discard");
  // jsdom writes a zero delay as 0ms
  const zero = /^-?0ms$/;
  assert.ok(discard.classList.contains("live"));
  assert.match(discard.style.animationDelay, zero);
  t.mock.timers.tick(2_000);
  page.push(makeView(makeGame()));
  assert.equal(page.$("discard").style.animationDelay, "-2000ms");
  // a new discard starts its glow over
  page.push(makeView(makeGame({ discard_seq: 3, discard_top: 7 })));
  assert.match(page.$("discard").style.animationDelay, zero);
  page.push(makeView(makeGame({ discard_seq: 3, discard_top: 7, matchable: false })));
  assert.equal(page.$("discard").classList.contains("live"), false);
  assert.equal(page.$("discard").style.animationDelay, "");
});

function scoredGame(extra = {}) {
  return makeGame({
    status: "scored",
    seats: [
      seat("p0", [1, 2, 3, 4], { score: 10, won: false, total: 42, carry: 32, tally: { matches: 2, misses: 1, penalties: 1, specials: 3 } }),
      seat("p1", [0, 0, 1, -1], { score: 0, won: true, total: 25, carry: 25, match_won: true, tally: { matches: 1, misses: 0, penalties: 0, specials: 0 } }),
      seat("p2", [], { forfeited: true, score: null, total: 30, carry: 30 }),
    ],
    match_over: true,
    ...extra,
  });
}

test("the end of a game sums up each player and the match (UI-38)", async (t) => {
  const history = [
    { m: 1, over: false, totals: { p0: 20, p1: 15, p2: 30 }, scores: {} },
    { m: 1, over: false, totals: { p0: 32, p1: 25, p2: 30 }, scores: {} },
    { m: 1, over: true, totals: { p0: 42, p1: 25 }, scores: {} },
  ];
  const view = makeView(scoredGame(), { room: room({ target_score: 40 }, history),
    members: [member("p0", "ada"), member("p1", "bob"), member("p2", "cy")] });
  const page = await boot({ view });
  t.after(page.stop);
  const summary = page.$("summary");
  assert.equal(summary.hidden, false);
  assert.equal(summary.querySelector(".match-result").textContent, "match over: bob won the match. the next game starts a new one.");
  const rows = [...summary.querySelectorAll("tr")].map((r) => [...r.children].map((c) => c.textContent));
  assert.deepEqual(rows, [
    ["player", "score", "total", "matches", "missed", "penalties", "specials"],
    ["bob won, won the match", "0", "25", "1", "0", "0", "0"],
    ["you", "10", "42", "2", "1", "1", "3"],
    ["cy", "out", "30", "0", "0", "0", "0"],
  ]);
  const chart = summary.querySelector(".chart");
  assert.equal(chart.querySelector("svg").getAttribute("aria-label"), "running totals over 3 games");
  const lines = [...chart.querySelectorAll("polyline")];
  assert.equal(lines.length, 3);
  assert.deepEqual(lines.map((l) => l.getAttribute("stroke")), [COLORS[0], COLORS[1], COLORS[2]]);
  // cy left after two games
  assert.equal(lines[2].getAttribute("points").split(" ").length, 2);
  assert.equal(chart.querySelector("text").textContent, "40");
  assert.deepEqual([...chart.querySelectorAll("figcaption > span")].map((s) => s.textContent), ["you", "bob", "cy"]);

  // the next game hides it
  page.push(makeView(makeGame({ id: 2 }), { room: room({ target_score: 40 }, history) }));
  assert.equal(page.$("summary").hidden, true);
});

test("without a target the summary skips totals until there are two games", async (t) => {
  const g = scoredGame({ match_over: false });
  g.seats[1].match_won = false;
  const one = [{ m: 1, over: false, totals: { p0: 10, p1: 0 }, scores: {} }];
  const page = await boot({ view: makeView(g, { room: room({}, one) }) });
  t.after(page.stop);
  const summary = page.$("summary");
  assert.equal(summary.querySelector(".match-result"), null);
  assert.equal(summary.querySelector("tr").children.length, 6);
  assert.equal(summary.querySelector(".chart"), null);
  // a player no longer seated is drawn in grey, with no target line
  const two = [...one, { m: 1, over: false, totals: { p0: 20, p1: 0, p9: 4 }, scores: {} }];
  page.push(makeView(g, { room: room({}, two) }));
  assert.equal(summary.querySelector("tr").children.length, 7);
  assert.equal(summary.querySelector(".chart text"), null);
  assert.equal(summary.querySelectorAll(".chart polyline")[2].getAttribute("stroke"), "#999");
  // a room playing to a target says the match goes on
  page.push(makeView(g, { room: room({ target_score: 100 }, two) }));
  assert.equal(summary.querySelector(".match-result").textContent, "playing to 100. the match goes on.");
  // an old room view without history still sums up the game
  const bare = makeView(g);
  page.push(bare);
  assert.equal(summary.querySelector(".chart"), null);
});

test("hands show the running total toward the target during a game", async (t) => {
  const g = makeGame({ seats: [seat("p0", ["x", "x", "x", "x"], { carry: 32 }), seat("p1")] });
  const page = await boot({ view: makeView(g, { room: room({ target_score: 100 }) }) });
  t.after(page.stop);
  const totals = [...page.doc.querySelectorAll(".hand .total")].map((s) => s.textContent);
  assert.deepEqual(totals.sort(), ["0/100", "32/100"]);
  page.push(makeView(g, { room: room({}) }));
  assert.equal(page.doc.querySelector(".hand .total"), null);
});

test("matches say how fast they were, and a lost race says by how much (UI-43)", async (t) => {
  const page = await boot();
  t.after(page.stop);
  const s = page.live();
  page.push(makeView(makeGame(), { events: [ev(1, "match", "p1", { ok: true, seat: 0, slot: 2, value: 4, reaction_ms: 180 })] }));
  assert.equal(page.$("log").querySelector("li").textContent, "bob matched your 4 in 180ms");
  page.slot(1, 0).click();
  page.ok();
  const ref = s.sent.at(-1).ref;
  s.receive({ type: "result", ref, ok: false, code: "too_late", message: "too late: a match 120ms faster got there first" });
  assert.equal(page.toast(), "too late: a match 120ms faster got there first");
  s.receive({ type: "result", ref: 99, ok: false, code: "too_late", message: "that discard was already matched or covered" });
  assert.equal(page.toast(), "too late, someone matched first");
});

test("the log names the match winners when a game ends the match", async (t) => {
  const page = await boot({ view: makeView(makeGame(), {
    events: [
      ev(2, "scored", null, { winners: ["p0"], match_over: true, match_winners: ["p1"] }),
      ev(1, "scored", null, { winners: ["p0"], match_over: false, match_winners: [] }),
    ],
  }) });
  t.after(page.stop);
  const lines = [...page.$("log").querySelectorAll("li")].map((l) => l.textContent);
  assert.deepEqual(lines, ["game over: you; match won by bob", "game over: you"]);
});
