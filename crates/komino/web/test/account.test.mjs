import { test } from "node:test";
import assert from "node:assert/strict";
import { boot, flush, makeView, member } from "../helpers.mjs";

const signedIn = { id: "p0", name: "ada", username: "ada" };

function rename(page, value) {
  page.$("name").value = value;
  page.$("name").dispatchEvent(new page.w.Event("change"));
  return flush();
}

function submit(page) {
  page.$("account-form").dispatchEvent(new page.w.Event("submit", { cancelable: true }));
  return flush();
}

test("an anonymous player is offered sign in", async (t) => {
  const page = await boot({ path: "/komino" });
  t.after(page.stop);
  assert.equal(page.$("account-btn").textContent, "sign in");
  assert.equal(page.$("account-user").hidden, true);
  assert.equal(page.$("account").hidden, true);
});

test("a signed in player sees their username and can sign out", async (t) => {
  const page = await boot({
    path: "/komino",
    me: signedIn,
    routes: { "POST /komino/api/logout": [200, { ok: true }] },
  });
  t.after(page.stop);
  assert.equal(page.$("account-user").hidden, false);
  assert.equal(page.$("account-user").textContent, "signed in as ada");
  assert.equal(page.$("account-btn").textContent, "sign out");
  page.$("account-btn").click();
  await flush();
  assert.equal(page.callsTo("POST /komino/api/logout").length, 1);
  // the page loads again as whoever the cookies now say
  assert.equal(page.location.href, "/komino");
});

test("a failed sign out says why and stays", async (t) => {
  const page = await boot({
    path: "/komino",
    me: signedIn,
    routes: { "POST /komino/api/logout": [500, { message: "database error" }] },
  });
  t.after(page.stop);
  page.$("account-btn").click();
  await flush();
  assert.equal(page.toast(), "database error");
  assert.equal(page.location.href, "https://kominick.com/komino");
});

test("signing in sends the username and password and reloads the room", async (t) => {
  const page = await boot({ routes: { "POST /komino/api/login": [200, signedIn] } });
  t.after(page.stop);
  page.$("account-btn").click();
  assert.equal(page.$("account").hidden, false);
  assert.equal(page.$("account-title").textContent, "sign in");
  assert.equal(page.$("account-name-row").hidden, true);
  assert.equal(page.$("account-password").getAttribute("autocomplete"), "current-password");
  assert.equal(page.doc.activeElement, page.$("account-username"));
  page.$("account-username").value = "ada";
  page.$("account-password").value = "correct horse";
  await submit(page);
  assert.deepEqual(page.callsTo("POST /komino/api/login").at(-1).body, { username: "ada", password: "correct horse" });
  assert.equal(page.location.href, "/komino/r/ABCDEF");
});

test("a wrong password shows in the dialog and can be tried again", async (t) => {
  const page = await boot({
    path: "/komino",
    routes: { "POST /komino/api/login": [401, { code: "unauthorized", message: "wrong username or password" }] },
  });
  t.after(page.stop);
  page.$("account-btn").click();
  page.$("account-username").value = "ada";
  page.$("account-password").value = "nope nope";
  await submit(page);
  assert.equal(page.$("account-error").textContent, "wrong username or password");
  assert.equal(page.$("account-submit").disabled, false);
  assert.equal(page.$("account").hidden, false);
  assert.equal(page.location.href, "https://kominick.com/komino");
});

test("creating an account sends the optional display name", async (t) => {
  const page = await boot({ path: "/komino", routes: { "POST /komino/api/signup": [200, signedIn] } });
  t.after(page.stop);
  page.$("account-btn").click();
  page.$("account-switch").click();
  assert.equal(page.$("account-title").textContent, "create an account");
  assert.equal(page.$("account-submit").textContent, "create account");
  assert.equal(page.$("account-name-row").hidden, false);
  assert.equal(page.$("account-password").getAttribute("autocomplete"), "new-password");
  assert.match(page.$("account-about").textContent, /rooms, and stats/);
  page.$("account-username").value = "ada";
  page.$("account-password").value = "correct horse";
  page.$("account-name").value = "Ada L";
  await submit(page);
  assert.deepEqual(page.callsTo("POST /komino/api/signup").at(-1).body, {
    username: "ada",
    password: "correct horse",
    name: "Ada L",
  });
  assert.equal(page.location.href, "/komino");
});

test("a taken username shows, and switching back clears it", async (t) => {
  const page = await boot({
    path: "/komino",
    routes: { "POST /komino/api/signup": [409, { code: "taken", message: "that username is taken" }] },
  });
  t.after(page.stop);
  page.$("account-btn").click();
  page.$("account-switch").click();
  await submit(page);
  assert.equal(page.$("account-error").textContent, "that username is taken");
  page.$("account-switch").click();
  assert.equal(page.$("account-title").textContent, "sign in");
  assert.equal(page.$("account-error").textContent, "");
});

test("the account dialog closes by button, backdrop, or escape, forgetting the password", async (t) => {
  const page = await boot({ path: "/komino" });
  t.after(page.stop);
  const open = () => {
    page.$("account-btn").click();
    page.$("account-password").value = "secret pw";
  };
  open();
  page.$("account-close").click();
  assert.equal(page.$("account").hidden, true);
  assert.equal(page.$("account-password").value, "");
  open();
  page.$("account").dispatchEvent(new page.w.Event("click", { bubbles: true }));
  assert.equal(page.$("account").hidden, true);
  open();
  // a click inside the dialog keeps it open
  page.$("account-title").dispatchEvent(new page.w.Event("click", { bubbles: true }));
  assert.equal(page.$("account").hidden, false);
  page.key("Escape");
  assert.equal(page.$("account").hidden, true);
  assert.equal(page.$("account-password").value, "");
});

test("game keys do nothing while the account dialog is open", async (t) => {
  const page = await boot();
  t.after(page.stop);
  page.push(makeView());
  page.$("account-btn").click();
  page.key("d");
  assert.equal(page.live().sent.length, 0);
});

test("in a room the name field edits the room name", async (t) => {
  const renamed = makeView(undefined, { members: [member("p0", "the countess"), member("p1", "bob")] });
  const cleared = makeView(undefined, { members: [member("p0", "ada"), member("p1", "bob")] });
  let reply = [200, renamed];
  const page = await boot({ routes: { "POST /komino/api/rooms/ABCDEF/name": () => reply } });
  t.after(page.stop);
  assert.equal(page.$("name-label").textContent, "name here");
  assert.match(page.$("name").title, /this room/);
  await rename(page, "the countess");
  assert.deepEqual(page.calls.at(-1), { key: "POST /komino/api/rooms/ABCDEF/name", body: { name: "the countess" } });
  assert.equal(page.$("name").value, "the countess");
  assert.equal(page.callsTo("POST /komino/api/me").length, 0);
  // a blank name shows the default again
  reply = [200, cleared];
  await rename(page, "");
  assert.equal(page.$("name").value, "ada");
  reply = [400, { code: "invalid", message: "name must be at most 24 characters" }];
  await rename(page, "x".repeat(30));
  assert.equal(page.toast(), "name must be at most 24 characters");
});

test("the room name field follows the view, but not while typing", async (t) => {
  const page = await boot();
  t.after(page.stop);
  page.push(makeView(undefined, { members: [member("p0", "countess"), member("p1", "bob")] }));
  assert.equal(page.$("name").value, "countess");
  page.$("name").focus();
  page.$("name").value = "half typed";
  page.push(makeView(undefined, { members: [member("p0", "duchess"), member("p1", "bob")] }));
  assert.equal(page.$("name").value, "half typed");
});

test("a watch page edits the default name", async (t) => {
  const page = await boot({
    path: "/komino/r/ABCDEF/watch",
    view: makeView(null, { me: null, observer: true }),
    routes: { "POST /komino/api/me": [200, { id: "p0", name: "grace" }] },
  });
  t.after(page.stop);
  assert.equal(page.$("name-label").textContent, "name");
  assert.equal(page.$("name").value, "ada");
  await rename(page, "grace");
  assert.equal(page.callsTo("POST /komino/api/me").length, 1);
  assert.equal(page.$("name").value, "grace");
});

function submitForm(page, id) {
  page.$(id).dispatchEvent(new page.w.Event("submit", { cancelable: true }));
  return flush();
}

test("the header username links to the account page", async (t) => {
  const page = await boot({ path: "/komino", me: signedIn });
  t.after(page.stop);
  assert.equal(page.$("account-user").getAttribute("href"), "/komino/account");
});

test("the account page asks an anonymous player to sign in", async (t) => {
  const page = await boot({ path: "/komino/account" });
  t.after(page.stop);
  assert.equal(page.$("account-page").hidden, false);
  assert.equal(page.$("lobby").hidden, true);
  assert.equal(page.$("room").hidden, true);
  assert.equal(page.$("acct-anon").hidden, false);
  assert.equal(page.$("acct-who").hidden, true);
  assert.equal(page.$("acct-body").hidden, true);
  page.$("acct-signin").click();
  assert.equal(page.$("account").hidden, false);
  assert.equal(page.$("account-title").textContent, "sign in");
});

const info = (over = {}) => ({ id: "p0", name: "ada", username: "ada", sessions: 1, recovery: true, rooms: [], ...over });

test("the account page lists your rooms, for an anonymous player too", async (t) => {
  const rooms = [
    { code: "ABCDEF", host: true, members: 3, playing: true },
    { code: "GHJKMN", host: false, members: 1, playing: false },
    { code: "<b>x</b>", host: false, members: 2, playing: false },
  ];
  const page = await boot({
    path: "/komino/account",
    routes: { "GET /komino/api/account": [200, info({ username: null, sessions: 0, recovery: false, rooms })] },
  });
  t.after(page.stop);
  const items = [...page.$("acct-rooms").querySelectorAll("li")];
  assert.deepEqual(
    items.map((li) => li.textContent),
    ["ABCDEF host, 3 players, playing", "GHJKMN 1 player", "<b>x</b> 2 players"],
  );
  assert.equal(items[0].querySelector("a").getAttribute("href"), "/komino/r/ABCDEF");
  assert.equal(page.$("acct-rooms").querySelector("b"), null);
  // the account only parts stay empty
  assert.equal(page.$("sessions-text").textContent, "");

  const none = await boot({ path: "/komino/account", routes: { "GET /komino/api/account": [200, info()] } });
  t.after(none.stop);
  assert.equal(none.$("acct-rooms").textContent, "none right now");

  const failed = await boot({ path: "/komino/account", routes: { "GET /komino/api/account": [500, { message: "database error" }] } });
  t.after(failed.stop);
  assert.equal(failed.$("rooms-error").textContent, "database error");
});

test("an account sees its devices and can sign the others out", async (t) => {
  let sessions = 3;
  const page = await boot({
    path: "/komino/account",
    me: signedIn,
    routes: {
      "GET /komino/api/account": () => [200, info({ sessions })],
      "POST /komino/api/account/sign-out-others": () => {
        sessions = 1;
        return [200, { ended: 2 }];
      },
    },
  });
  t.after(page.stop);
  assert.equal(page.$("acct-who").hidden, false);
  assert.equal(page.$("acct-sessions").textContent, "(3 devices)");
  assert.match(page.$("sessions-text").textContent, /3 devices, including this one/);
  assert.equal(page.$("sessions-submit").disabled, false);
  page.$("sessions-submit").click();
  await flush();
  assert.equal(page.callsTo("POST /komino/api/account/sign-out-others").length, 1);
  assert.equal(page.toast(), "signed out 2 other devices");
  assert.equal(page.$("acct-sessions").textContent, "(1 device)");
  // nothing left to sign out
  assert.equal(page.$("sessions-submit").disabled, true);
});

test("a failed sign out of other devices says why", async (t) => {
  const page = await boot({
    path: "/komino/account",
    me: signedIn,
    routes: {
      "GET /komino/api/account": [200, info({ sessions: 2 })],
      "POST /komino/api/account/sign-out-others": [500, { message: "database error" }],
    },
  });
  t.after(page.stop);
  page.$("sessions-submit").click();
  await flush();
  assert.equal(page.$("sessions-error").textContent, "database error");
});

test("the account page changes the username", async (t) => {
  let reply = [409, { code: "taken", message: "that username is taken" }];
  const page = await boot({
    path: "/komino/account",
    me: signedIn,
    routes: { "POST /komino/api/account/username": () => reply },
  });
  t.after(page.stop);
  page.$("un-new").value = "grace";
  page.$("un-password").value = "correct horse";
  await submitForm(page, "username-form");
  assert.deepEqual(page.callsTo("POST /komino/api/account/username").at(-1).body, {
    username: "grace",
    password: "correct horse",
  });
  assert.equal(page.$("un-error").textContent, "that username is taken");
  assert.equal(page.$("un-submit").disabled, false);
  reply = [200, { id: "p0", name: "countess", username: "countess" }];
  page.$("un-new").value = "countess";
  await submitForm(page, "username-form");
  assert.equal(page.$("un-error").textContent, "");
  assert.equal(page.$("un-new").value, "");
  assert.equal(page.$("un-password").value, "");
  assert.equal(page.$("account-user").textContent, "signed in as countess");
  assert.equal(page.$("acct-username").textContent, "countess");
  assert.equal(page.$("pw-username").value, "countess");
  assert.equal(page.$("name").value, "countess");
  assert.equal(page.toast(), "username is now countess");
});

test("the account page makes a new recovery code and shows it once", async (t) => {
  let reply = [401, { code: "unauthorized", message: "wrong password" }];
  const page = await boot({
    path: "/komino/account",
    me: signedIn,
    routes: {
      "GET /komino/api/account": [200, info({ recovery: false })],
      "POST /komino/api/account/recovery": () => reply,
    },
  });
  t.after(page.stop);
  assert.match(page.$("recovery-status").textContent, /no recovery code/);
  page.$("recovery-password").value = "wrong horse";
  await submitForm(page, "recovery-form");
  assert.equal(page.$("recovery-error").textContent, "wrong password");
  assert.equal(page.$("recovery").hidden, true);
  reply = [200, { recovery_code: "abcde-fghjk-mnpqr-stvwx-yz012" }];
  page.$("recovery-password").value = "correct horse";
  await submitForm(page, "recovery-form");
  assert.deepEqual(page.callsTo("POST /komino/api/account/recovery").at(-1).body, { password: "correct horse" });
  assert.equal(page.$("recovery-password").value, "");
  assert.equal(page.$("recovery").hidden, false);
  assert.equal(page.$("recovery-code").textContent, "abcde-fghjk-mnpqr-stvwx-yz012");
  page.$("recovery-copy").click();
  await flush();
  assert.deepEqual(page.clipboard, ["abcde-fghjk-mnpqr-stvwx-yz012"]);
  assert.equal(page.toast(), "recovery code copied");
  // escape does not dismiss it
  page.key("Escape");
  assert.equal(page.$("recovery").hidden, false);
  const loads = page.callsTo("GET /komino/api/account").length;
  page.$("recovery-done").click();
  await flush();
  assert.equal(page.$("recovery").hidden, true);
  assert.equal(page.$("recovery-code").textContent, "");
  assert.equal(page.callsTo("GET /komino/api/account").length, loads + 1);
});

test("a new account sees its recovery code before the page reloads", async (t) => {
  const page = await boot({
    path: "/komino",
    routes: { "POST /komino/api/signup": [200, { ...signedIn, recovery_code: "abcde-fghjk-mnpqr-stvwx-yz012" }] },
    clipboardFails: true,
  });
  t.after(page.stop);
  page.$("account-btn").click();
  page.$("account-switch").click();
  page.$("account-username").value = "ada";
  page.$("account-password").value = "correct horse";
  await submit(page);
  assert.equal(page.$("account").hidden, true);
  assert.equal(page.$("account-password").value, "");
  assert.equal(page.$("recovery").hidden, false);
  assert.equal(page.$("recovery-code").textContent, "abcde-fghjk-mnpqr-stvwx-yz012");
  assert.equal(page.location.href, "https://kominick.com/komino");
  page.$("recovery-copy").click();
  await flush();
  assert.equal(page.toast(), "select the code to copy it");
  page.$("recovery-done").click();
  assert.equal(page.location.href, "/komino");
});

test("a forgotten password is reset with the recovery code", async (t) => {
  const page = await boot({
    path: "/komino",
    routes: {
      "POST /komino/api/account/recover": [200, { ...signedIn, recovery_code: "zzzzz-zzzzz-zzzzz-zzzzz-zzzzz" }],
    },
  });
  t.after(page.stop);
  page.$("account-btn").click();
  assert.equal(page.$("account-forgot").hidden, false);
  assert.equal(page.$("account-code-row").hidden, true);
  page.$("account-forgot").click();
  assert.equal(page.$("account-title").textContent, "use a recovery code");
  assert.equal(page.$("account-code-row").hidden, false);
  assert.equal(page.$("account-code").required, true);
  assert.equal(page.$("account-forgot").hidden, true);
  assert.equal(page.$("account-password-label").textContent, "new password");
  assert.equal(page.$("account-password").getAttribute("autocomplete"), "new-password");
  assert.match(page.$("account-about").textContent, /other devices are signed out/);
  page.$("account-username").value = "ada";
  page.$("account-code").value = "abcde fghjk mnpqr stvwx yz012";
  page.$("account-password").value = "battery staple";
  await submit(page);
  assert.deepEqual(page.callsTo("POST /komino/api/account/recover").at(-1).body, {
    username: "ada",
    password: "battery staple",
    code: "abcde fghjk mnpqr stvwx yz012",
  });
  assert.equal(page.$("account-code").value, "");
  assert.equal(page.$("recovery-code").textContent, "zzzzz-zzzzz-zzzzz-zzzzz-zzzzz");
  page.$("recovery-done").click();
  assert.equal(page.location.href, "/komino");
});

test("a wrong recovery code shows, and the dialog goes back to sign in", async (t) => {
  const page = await boot({
    path: "/komino",
    routes: {
      "POST /komino/api/account/recover": [401, { code: "unauthorized", message: "wrong username or recovery code" }],
    },
  });
  t.after(page.stop);
  page.$("account-btn").click();
  page.$("account-forgot").click();
  await submit(page);
  assert.equal(page.$("account-error").textContent, "wrong username or recovery code");
  assert.equal(page.$("recovery").hidden, true);
  assert.equal(page.$("account-switch").textContent, "have an account? sign in");
  page.$("account-switch").click();
  assert.equal(page.$("account-title").textContent, "sign in");
  assert.equal(page.$("account-code-row").hidden, true);
  assert.equal(page.$("account-code").required, false);
  assert.equal(page.$("account-password-label").textContent, "password");
  assert.equal(page.$("account-error").textContent, "");
});

test("game keys do nothing while a recovery code shows", async (t) => {
  const page = await boot({ routes: { "POST /komino/api/signup": [200, { ...signedIn, recovery_code: "c" }] } });
  t.after(page.stop);
  page.push(makeView());
  page.$("account-btn").click();
  page.$("account-switch").click();
  await submit(page);
  assert.equal(page.$("recovery").hidden, false);
  page.key("d");
  assert.equal(page.live().sent.length, 0);
});

test("the account page changes the password and clears the fields", async (t) => {
  let reply = [401, { code: "unauthorized", message: "wrong password" }];
  const page = await boot({
    path: "/komino/account",
    me: signedIn,
    routes: { "POST /komino/api/account/password": () => reply },
  });
  t.after(page.stop);
  assert.equal(page.$("acct-body").hidden, false);
  assert.equal(page.$("acct-username").textContent, "ada");
  assert.equal(page.$("pw-username").value, "ada");
  page.$("pw-current").value = "wrong horse";
  page.$("pw-new").value = "battery staple";
  await submitForm(page, "password-form");
  assert.deepEqual(page.callsTo("POST /komino/api/account/password").at(-1).body, {
    current: "wrong horse",
    new: "battery staple",
  });
  assert.equal(page.$("pw-error").textContent, "wrong password");
  assert.equal(page.$("pw-submit").disabled, false);
  assert.equal(page.$("pw-current").value, "wrong horse");
  reply = [200, signedIn];
  page.$("pw-current").value = "correct horse";
  await submitForm(page, "password-form");
  assert.equal(page.$("pw-error").textContent, "");
  assert.equal(page.$("pw-current").value, "");
  assert.equal(page.$("pw-new").value, "");
  assert.match(page.toast(), /other devices are signed out/);
});

test("deleting the account confirms first, then leaves for the lobby", async (t) => {
  const page = await boot({
    path: "/komino/account",
    me: signedIn,
    routes: { "POST /komino/api/account/delete": [200, { ok: true }] },
  });
  t.after(page.stop);
  page.$("delete-password").value = "correct horse";
  await submitForm(page, "delete-form");
  assert.match(page.confirms.at(-1), /for good/);
  assert.deepEqual(page.callsTo("POST /komino/api/account/delete").at(-1).body, { password: "correct horse" });
  assert.equal(page.location.href, "/komino");
});

test("a declined or failed delete keeps the account", async (t) => {
  const declined = await boot({ path: "/komino/account", me: signedIn, confirmAnswer: false });
  t.after(declined.stop);
  await submitForm(declined, "delete-form");
  assert.equal(declined.callsTo("POST /komino/api/account/delete").length, 0);

  const page = await boot({
    path: "/komino/account",
    me: signedIn,
    routes: { "POST /komino/api/account/delete": [401, { code: "unauthorized", message: "wrong password" }] },
  });
  t.after(page.stop);
  await submitForm(page, "delete-form");
  assert.equal(page.$("delete-error").textContent, "wrong password");
  assert.equal(page.$("delete-submit").disabled, false);
  assert.equal(page.location.href, "https://kominick.com/komino/account");
});

const lifetime = {
  games_played: 8,
  wins: 3,
  komino_calls: 2,
  komino_wins: 1,
  matches: 12,
  failed_matches: 2,
  special_moves: 5,
  cards_interacted: 90,
  forfeits: 0,
  draws: 30,
  swaps: 25,
  views: 6,
  others_matched: 4,
  bot_games: 2,
  match_time: { timed: 12, average_ms: 812, median_ms: 725 },
  partners: [
    { id: "p1", name: "bob", username: null, games: 5 },
    { id: "p2", name: "Grace H", username: "grace", games: 1 },
    { id: "p3", name: "<b>x</b>", username: "<b>x</b>", games: 1 },
  ],
};

function statValue(page, label) {
  const item = [...page.$("stats-totals").querySelectorAll("div")].find((d) => d.querySelector("dt").textContent === label);
  return item && item.querySelector("dd").textContent;
}

test("the account page shows lifetime stats and partners", async (t) => {
  const page = await boot({
    path: "/komino/account",
    me: signedIn,
    routes: { "GET /komino/api/account/stats": [200, lifetime] },
  });
  t.after(page.stop);
  assert.equal(page.callsTo("GET /komino/api/account/stats").length, 1);
  assert.equal(statValue(page, "games played"), "8");
  assert.equal(statValue(page, "games won"), "3 (38%)");
  assert.equal(statValue(page, "cards drawn"), "30");
  assert.equal(statValue(page, "cards swapped"), "25");
  assert.equal(statValue(page, "cards viewed"), "6");
  assert.equal(statValue(page, "matches"), "12");
  assert.equal(statValue(page, "matched from others"), "4");
  assert.equal(statValue(page, "missed matches"), "2");
  assert.equal(statValue(page, "average match time"), "0.81s");
  assert.equal(statValue(page, "median match time"), "0.73s");
  assert.equal(statValue(page, "games with bots"), "2");
  const rows = [...page.$("stats-partners").querySelectorAll("li")].map((li) => li.textContent);
  assert.deepEqual(rows, ["bob 5 games", "Grace H (grace) 1 game", "<b>x</b> 1 game"]);
  // names are text, never markup
  assert.equal(page.$("stats-partners").querySelector("b"), null);
});

test("stats before any game, and a median in the top band", async (t) => {
  const none = {
    ...lifetime,
    games_played: 0,
    wins: 0,
    match_time: { timed: 0, average_ms: null, median_ms: null },
    partners: [],
  };
  const page = await boot({ path: "/komino/account", me: signedIn, routes: { "GET /komino/api/account/stats": [200, none] } });
  t.after(page.stop);
  assert.equal(statValue(page, "games won"), "0");
  assert.equal(statValue(page, "average match time"), "-");
  assert.equal(statValue(page, "median match time"), "-");
  assert.equal(page.$("stats-partners").textContent, "nobody yet");

  const slow = { ...lifetime, match_time: { timed: 3, average_ms: 6400, median_ms: 5000 } };
  const page2 = await boot({ path: "/komino/account", me: signedIn, routes: { "GET /komino/api/account/stats": [200, slow] } });
  t.after(page2.stop);
  assert.equal(statValue(page2, "average match time"), "6.40s");
  assert.equal(statValue(page2, "median match time"), "5s+");
});

test("stats that fail to load say so, and anonymous players see theirs too", async (t) => {
  const page = await boot({
    path: "/komino/account",
    me: signedIn,
    routes: { "GET /komino/api/account/stats": [500, { message: "database error" }] },
  });
  t.after(page.stop);
  assert.equal(page.$("stats-error").textContent, "database error");
  assert.equal(page.$("stats-totals").innerHTML, "");
  const anon = await boot({ path: "/komino/account", routes: { "GET /komino/api/account/stats": [200, lifetime] } });
  t.after(anon.stop);
  assert.equal(anon.callsTo("GET /komino/api/account/stats").length, 1);
  assert.equal(statValue(anon, "games played"), "8");
});

test("the cookie notice shows until acknowledged", async (t) => {
  const first = await boot({ path: "/komino" });
  t.after(first.stop);
  assert.equal(first.$("cookies").hidden, false);
  assert.match(first.$("cookies").textContent, /no tracking/);
  first.$("cookies-ok").click();
  assert.equal(first.$("cookies").hidden, true);
  assert.equal(first.w.localStorage.getItem("komino.cookies"), "ok");
});

test("the cookie notice stays hidden once acknowledged", async (t) => {
  const store = new Map([["komino.cookies", "ok"]]);
  const storage = { getItem: (k) => (store.has(k) ? store.get(k) : null), setItem: (k, v) => store.set(k, v) };
  const page = await boot({ path: "/komino", storage });
  t.after(page.stop);
  assert.equal(page.$("cookies").hidden, true);
});

test("the cookie notice still works without storage", async (t) => {
  const broken = {
    getItem() {
      throw new Error("blocked");
    },
    setItem() {
      throw new Error("blocked");
    },
  };
  const page = await boot({ path: "/komino", storage: broken });
  t.after(page.stop);
  assert.equal(page.$("cookies").hidden, false);
  page.$("cookies-ok").click();
  assert.equal(page.$("cookies").hidden, true);
});
