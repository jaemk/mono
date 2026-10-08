// Bots (BOT-1, BOT-5): the host adds them from the players panel, at a level.
import { test } from "node:test";
import assert from "node:assert/strict";
import { boot, flush, makeView, member } from "../helpers.mjs";

const withBots = () => makeView(undefined, {
  members: [member("p0", "ada"), member("p1", "bob"), member("b1", "bot ava", { bot: true, bot_level: "hard" }),
    member("b2", "bot ben", { bot: true })],
});

test("the host adds a bot at the chosen level, which the players list tags", async (t) => {
  const page = await boot({ routes: { "POST /komino/api/rooms/ABCDEF/bots": [200, withBots()] } });
  t.after(page.stop);
  assert.equal(page.$("bots").hidden, false);
  assert.equal(page.$("bot-level").value, "normal");
  page.$("bot-level").value = "hard";
  page.$("add-bot").click();
  await flush();
  assert.deepEqual(page.callsTo("POST /komino/api/rooms/ABCDEF/bots").map((c) => c.body), [{ level: "hard" }]);
  assert.equal(page.toast(), "bot added; it plays from the next game");
  const tags = [...page.$("members").querySelectorAll(".tag")].map((s) => s.textContent);
  // a bot saved without a level plays at normal
  assert.deepEqual(tags.slice(2), ["bot, hard", "bot, normal"]);
});

test("a refused bot says why, and only the host sees the button", async (t) => {
  const page = await boot({ routes: { "POST /komino/api/rooms/ABCDEF/bots": [400, { code: "invalid", message: "a room can have at most 7 bots" }] } });
  t.after(page.stop);
  page.$("add-bot").click();
  await flush();
  assert.equal(page.toast(), "a room can have at most 7 bots");

  const guest = await boot({ me: { id: "p1", name: "bob" }, view: makeView(undefined, { me: "p1" }) });
  t.after(guest.stop);
  assert.equal(guest.$("bots").hidden, true);
  const watch = await boot({ path: "/komino/r/ABCDEF/watch", view: makeView(undefined, { me: null, observer: true }) });
  t.after(watch.stop);
  assert.equal(watch.$("bots").hidden, true);
});
