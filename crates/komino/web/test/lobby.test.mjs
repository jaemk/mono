import { test } from "node:test";
import assert from "node:assert/strict";
import { boot, flush } from "../helpers.mjs";

test("the lobby shows on /komino with the player's name", async (t) => {
  const page = await boot({ path: "/komino" });
  t.after(page.stop);
  assert.equal(page.$("lobby").hidden, false);
  assert.equal(page.$("room").hidden, true);
  assert.equal(page.$("name").value, "ada");
  assert.equal(page.sockets.length, 0);
});

test("create a room goes to its page", async (t) => {
  const page = await boot({
    path: "/komino",
    routes: { "POST /komino/api/rooms": [200, { room: { code: "QWERTY" } }] },
  });
  t.after(page.stop);
  page.$("create").click();
  await flush();
  assert.equal(page.location.href, "/komino/r/QWERTY");
});

test("a failed create shows why", async (t) => {
  const page = await boot({
    path: "/komino",
    routes: { "POST /komino/api/rooms": [503, { code: "error", message: "could not allocate a room code" }] },
  });
  t.after(page.stop);
  page.$("create").click();
  await flush();
  assert.equal(page.toast(), "could not allocate a room code");
  assert.equal(page.location.href, "https://kominick.com/komino");
});

test("joining by code uppercases it, and a blank code does nothing", async (t) => {
  const page = await boot({ path: "/komino" });
  t.after(page.stop);
  const submit = () => page.$("join-form").dispatchEvent(new page.w.Event("submit", { cancelable: true }));
  page.$("join-code").value = "   ";
  submit();
  assert.equal(page.location.href, "https://kominick.com/komino");
  page.$("join-code").value = " abcdef ";
  submit();
  assert.equal(page.location.href, "/komino/r/ABCDEF");
});

test("renaming saves the trimmed name, and shows a rejection", async (t) => {
  let reply = [200, { id: "p0", name: "grace" }];
  const page = await boot({ path: "/komino", routes: { "POST /komino/api/me": () => reply } });
  t.after(page.stop);
  const rename = async (value) => {
    page.$("name").value = value;
    page.$("name").dispatchEvent(new page.w.Event("change"));
    await flush();
  };
  await rename("  grace ");
  assert.deepEqual(page.calls.at(-1), { key: "POST /komino/api/me", body: { name: "  grace " } });
  assert.equal(page.$("name").value, "grace");
  reply = [400, { code: "invalid", message: "name must not be blank" }];
  await rename("  ");
  assert.equal(page.toast(), "name must not be blank");
});

test("a failed identity load stops with a message", async (t) => {
  const page = await boot({ routes: { "GET /komino/api/me": [500, {}] } });
  t.after(page.stop);
  assert.equal(page.toast(), "request failed");
  assert.equal(page.$("room").hidden, true);
  assert.equal(page.sockets.length, 0);
});

test("toasts clear themselves", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const page = await boot({ routes: { "GET /komino/api/me": [500, { message: "down" }] } });
  t.after(page.stop);
  assert.equal(page.toast(), "down");
  t.mock.timers.tick(3000);
  assert.equal(page.toast(), null);
});
