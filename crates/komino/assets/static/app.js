// komino client. The server owns all state and sends a redacted view; this
// page renders it, turns taps into a select-then-confirm action, and sends
// the confirmed action over the room websocket. On a watch page it only
// renders.
//
// `createKomino(win)` takes the window (document, location, fetch, WebSocket,
// timers, confirm, navigator) so tests can drive it under jsdom; a browser
// starts it at the bottom of this file.
(function (root) {
  "use strict";

  const MOVES = { 7: "peek_own", 8: "peek_own", 9: "peek_other", 10: "peek_other", 11: "blind_swap", 12: "blind_swap", 13: "look_swap" };
  const MOVE_INFO = {
    peek_own: { color: "#2f6fd6", label: "peek own" },
    peek_other: { color: "#e07a1f", label: "peek other" },
    blind_swap: { color: "#7b4bc9", label: "blind swap" },
    look_swap: { color: "#d0393b", label: "look and swap" },
  };
  const MAX_BACKOFF = 10000;
  // how long a card stays marked after an event, and a flying card's trip
  const FX_MS = 1400;
  const FLY_MS = 700;
  const CAPTION_MS = 3000;
  // how long a missed card shows its value, in rooms that show misses
  const MISS_SHOW_MS = 3000;
  const SOUND_KEY = "komino.sound";

  const esc = (s) =>
    String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

  // ---------------------------------------------------------------- cards

  function eye(cx, cy, color) {
    return `<path d="M${cx - 15} ${cy} Q${cx} ${cy - 12} ${cx + 15} ${cy} Q${cx} ${cy + 12} ${cx - 15} ${cy} Z" fill="#fff" stroke="${color}" stroke-width="2.5"/>` +
      `<circle cx="${cx}" cy="${cy}" r="4.5" fill="${color}"/>`;
  }
  function outline(x, y, w, h, color, filled) {
    return `<rect x="${x}" y="${y}" width="${w}" height="${h}" rx="2.5" fill="${filled ? color : "#fff"}" stroke="${color}" stroke-width="2.5"/>`;
  }
  function crossing(y1, y2, color) {
    return `<path d="M22 ${y1} Q35 ${y1 - 12} 48 ${y1}" fill="none" stroke="${color}" stroke-width="2.5"/>` +
      `<polygon points="51,${y1 + 3} 42,${y1} 47,${y1 - 6}" fill="${color}"/>` +
      `<path d="M48 ${y2} Q35 ${y2 + 12} 22 ${y2}" fill="none" stroke="${color}" stroke-width="2.5"/>` +
      `<polygon points="19,${y2 - 3} 28,${y2} 23,${y2 + 6}" fill="${color}"/>`;
  }
  function symbol(mv) {
    const c = MOVE_INFO[mv].color;
    switch (mv) {
      case "peek_own":
        return outline(26, 48, 18, 25, c, false) + eye(35, 36, c);
      case "peek_other":
        return eye(35, 42, c) +
          `<line x1="20" y1="64" x2="46" y2="64" stroke="${c}" stroke-width="3"/>` +
          `<polygon points="45,57 55,64 45,71" fill="${c}"/>`;
      case "blind_swap":
        return outline(13, 40, 16, 22, c, true) + outline(41, 40, 16, 22, c, true) + crossing(34, 68, c);
      case "look_swap":
        return eye(35, 32, c) + crossing(56, 72, c);
    }
    return "";
  }

  // the bottom corner is the top corner turned about the card's center, so
  // both sit the same distance inside the edge
  function corner(v, ink, turned) {
    return `<text class="corner" x="7" y="17" font-size="14" font-weight="700" fill="${ink}"` +
      (turned ? ` transform="rotate(180 35 50)"` : "") + `>${v}</text>`;
  }

  function face(v) {
    const mv = MOVES[v];
    const ink = mv ? MOVE_INFO[mv].color : "#1f1d1a";
    const bg = mv ? "#fff" : v <= 0 ? "var(--card-low)" : "var(--card)";
    const center = mv
      ? symbol(mv)
      : `<text x="35" y="62" text-anchor="middle" font-size="34" font-weight="700" fill="${ink}">${v}</text>`;
    return `<svg viewBox="0 0 70 100" aria-hidden="true" font-family="system-ui, sans-serif">` +
      `<rect x="1" y="1" width="68" height="98" rx="7" fill="${bg}" stroke="${mv ? ink : "#bdb6a6"}" stroke-width="2"/>` +
      corner(v, ink, false) + corner(v, ink, true) + center + `</svg>`;
  }

  function back() {
    return `<svg viewBox="0 0 70 100" aria-hidden="true">` +
      `<rect x="1" y="1" width="68" height="98" rx="7" fill="var(--back)" stroke="#0003" stroke-width="2"/>` +
      `<rect x="7" y="7" width="56" height="86" rx="4" fill="none" stroke="var(--back-2)" stroke-width="2"/>` +
      `<path d="M35 20 L52 50 L35 80 L18 50 Z" fill="var(--back-2)"/></svg>`;
  }

  function cardLabel(v) {
    const mv = MOVES[v];
    return mv ? `${v}, ${MOVE_INFO[mv].label}` : String(v);
  }

  // ---------------------------------------------------------------- sealing
  //
  // Private card values arrive sealed to a short-lived client ECDH key
  // (spec SEAL-*): AES-256-GCM under HKDF-SHA256 of the P-256 shared secret
  // with the server's static key. Mirrors crates/komino/src/sealed.rs.

  const CURVE = { name: "ECDH", namedCurve: "P-256" };
  const INFO = new TextEncoder().encode("komino reveal v1");
  // how long one client key pair is used before a fresh one is made
  const KEY_MS = 5 * 60 * 1000;

  function b64d(s) {
    return Uint8Array.from(atob(s.replace(/-/g, "+").replace(/_/g, "/")), (c) => c.charCodeAt(0));
  }
  function b64e(buf) {
    return btoa(String.fromCharCode(...new Uint8Array(buf))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  }

  function importServerKey(subtle, b64) {
    return subtle.importKey("raw", b64d(b64), CURVE, false, []);
  }

  async function openSealed(subtle, serverKey, privateKey, aad, sealed) {
    const bits = await subtle.deriveBits({ name: "ECDH", public: serverKey }, privateKey, 256);
    const hkdf = await subtle.importKey("raw", bits, "HKDF", false, ["deriveKey"]);
    const aes = await subtle.deriveKey({ name: "HKDF", hash: "SHA-256", salt: b64d(sealed.salt), info: INFO },
      hkdf, { name: "AES-GCM", length: 256 }, false, ["decrypt"]);
    const plain = await subtle.decrypt({ name: "AES-GCM", iv: b64d(sealed.iv), additionalData: new TextEncoder().encode(aad) },
      aes, b64d(sealed.ct));
    return JSON.parse(new TextDecoder().decode(plain));
  }

  // ---------------------------------------------------------------- sound
  //
  // Cues are synthesized with Web Audio (UI-27): filtered noise for card
  // snaps and slides, short tones for everything else.

  function envelope(ctx, at, dur, peak) {
    const g = ctx.createGain();
    g.gain.setValueAtTime(0.0001, at);
    g.gain.exponentialRampToValueAtTime(peak, at + 0.008);
    g.gain.exponentialRampToValueAtTime(0.0001, at + dur);
    return g;
  }
  // a burst of noise swept through a band-pass, from f1 to f2 hz
  function noise(ctx, at, dur, f1, f2, peak) {
    const len = Math.ceil(ctx.sampleRate * dur);
    const buf = ctx.createBuffer(1, len, ctx.sampleRate);
    const data = buf.getChannelData(0);
    for (let i = 0; i < len; i++) data[i] = Math.random() * 2 - 1;
    const src = ctx.createBufferSource();
    src.buffer = buf;
    const band = ctx.createBiquadFilter();
    band.type = "bandpass";
    band.Q.value = 1.2;
    band.frequency.setValueAtTime(f1, at);
    band.frequency.exponentialRampToValueAtTime(f2, at + dur);
    src.connect(band).connect(envelope(ctx, at, dur, peak)).connect(ctx.destination);
    src.start(at);
    src.stop(at + dur);
  }
  function tone(ctx, at, dur, freq, peak, type, to) {
    const osc = ctx.createOscillator();
    osc.type = type;
    osc.frequency.setValueAtTime(freq, at);
    if (to) osc.frequency.exponentialRampToValueAtTime(to, at + dur);
    osc.connect(envelope(ctx, at, dur, peak)).connect(ctx.destination);
    osc.start(at);
    osc.stop(at + dur);
  }
  const SOUNDS = {
    flip: (ctx, t) => noise(ctx, t, 0.07, 3200, 1400, 0.5),
    slide: (ctx, t) => noise(ctx, t, 0.18, 600, 2400, 0.3),
    swap: (ctx, t) => {
      noise(ctx, t, 0.18, 600, 2400, 0.3);
      noise(ctx, t + 0.16, 0.18, 2400, 600, 0.3);
    },
    peek: (ctx, t) => {
      noise(ctx, t, 0.05, 2600, 1800, 0.25);
      tone(ctx, t + 0.03, 0.18, 880, 0.08, "sine");
    },
    match: (ctx, t) => {
      tone(ctx, t, 0.12, 660, 0.15, "sine");
      tone(ctx, t + 0.1, 0.22, 990, 0.15, "sine");
    },
    miss: (ctx, t) => tone(ctx, t, 0.28, 180, 0.12, "sawtooth", 110),
    komino: (ctx, t) => [523, 659, 784, 1047].forEach((f, i) => tone(ctx, t + i * 0.09, 0.28, f, 0.14, "triangle")),
    shuffle: (ctx, t) => {
      for (let i = 0; i < 6; i++) noise(ctx, t + i * 0.06, 0.06, 3000, 1600, 0.3);
    },
    turn: (ctx, t) => {
      tone(ctx, t, 0.35, 880, 0.12, "sine");
      tone(ctx, t + 0.12, 0.45, 1320, 0.08, "sine");
    },
  };

  // turn actions carry the turn token they were chosen against, so a stale
  // or repeated confirm is refused instead of applied to a moved-on turn
  const TURN_ACTIONS = ["draw", "take", "swap", "discard", "use_special", "komino", "peek", "blind_swap", "look_swap", "skip"];

  function createKomino(win) {
    const doc = win.document;
    const $ = (id) => doc.getElementById(id);

    const path = win.location.pathname.match(/^\/komino\/r\/([A-Za-z0-9]{6})(\/watch)?\/?$/) || [];
    // codes are case-insensitive, but the server seals reveals to the
    // canonical upper-case code, so it must match the decrypt aad
    const code = path[1] && path[1].toUpperCase();
    const watching = Boolean(path[2]);
    let view = null;
    let me = null;
    let ws = null;
    let backoff = 500;
    let sel = null; // pending selection: { key, kind, ...fields }
    let matchMode = false;
    let ticker = null;
    let closed = false;
    let skew = 0; // server clock minus this clock, from the last view
    let refCounter = 0;
    // when this page first showed the current discard, to report a match's
    // reaction time (RT-18): { game, seq, at }
    let shown = null;
    // a sent match waiting for its window to settle (RT-16): { ref, seq, seat, slot }
    let claiming = null;
    const clock = () => (win.performance ? win.performance.now() : Date.now());
    // the newest event played, { game, id }; null until the first view (UI-23)
    let seenEvent = null;
    // cards and piles marked by recent events: "s:<seat>:<slot>", "deck",
    // "discard", "status" -> { cls, at }
    const fx = new Map();
    // cards flying between two places, launched once the table is drawn
    let flights = [];
    let audio = null;
    let sound = loadSound();
    // the last markup written per element, so an unchanged render leaves the
    // buttons in place and a tap that spans it still lands
    const painted = new Map();
    let controlHandlers = [];

    const subtle = win.crypto.subtle;
    let serverKey = null; // { kid, key }, fetched once per page (SEAL-4)
    let clientKey = null; // { pair, pub, born }, replaced every KEY_MS (SEAL-5)
    // decrypted private values, only while they are shown (SEAL-9):
    // "o:<game>" opening, "d:<game>:<turn_seq>" drawn, "p:<id>" a peek
    const secrets = new Map();
    const asked = new Set(); // secret keys already requested

    // ---------------------------------------------------------------- helpers

    function nameOf(id) {
      const m = ((view && view.members) || []).find((p) => p.id === id);
      return m ? m.name : "someone";
    }
    function game() {
      return view && view.game;
    }
    function inPlay(g) {
      return g && (g.status === "playing" || g.status === "final");
    }
    function myTurn(g) {
      return inPlay(g) && g.me !== null && g.turn === g.me;
    }
    function seatOwner(g, seat) {
      return seat === g.me ? "your" : `${nameOf(g.seats[seat].player)}'s`;
    }
    function canMatch(g) {
      return g && g.me !== null && !g.seats[g.me].forfeited && g.matchable && g.discard_top !== null &&
        ["playing", "final", "scoring"].includes(g.status);
    }
    function filled(g, seat, slot) {
      const s = g.seats[seat] && g.seats[seat].slots[slot];
      return s !== null && s !== undefined;
    }
    function turnKey(g) {
      return `t:${g.id}:${g.turn_seq}:${g.status}:${g.turn}:${JSON.stringify(g.stage)}`;
    }
    function matchKey(g) {
      return `m:${g.id}:${g.discard_seq}:${g.matchable}`;
    }
    // peeks end at the server's deadline, read against the server clock; a
    // peek with no deadline lasts until hidden (SET-11)
    function live(until) {
      return until === null || until === undefined || Date.now() + skew < until;
    }
    // hands are two rows, the second nearest the player (SET-5)
    function columns(g) {
      return Math.ceil((g.hand_size || 4) / 2);
    }
    // ---------------------------------------------------------------- secrets

    async function loadServerKey() {
      const k = await api("GET", "/komino/api/key");
      serverKey = { kid: k.kid, key: await importServerKey(subtle, k.public_key) };
    }
    async function myKey() {
      if (!clientKey || Date.now() - clientKey.born >= KEY_MS) {
        // non-extractable: the private half never leaves WebCrypto
        const pair = await subtle.generateKey(CURVE, false, ["deriveBits"]);
        clientKey = { pair, pub: b64e(await subtle.exportKey("raw", pair.publicKey)), born: Date.now() };
      }
      return clientKey;
    }
    // ask for one private value and open it with this page's key
    async function reveal(body) {
      let ck = await myKey();
      const ask = () => api("POST", `/komino/api/rooms/${code}/reveal`, Object.assign({ client_key: ck.pub }, body));
      let sealed;
      try {
        sealed = await ask();
      } catch (err) {
        if (err.code !== "key_expired") throw err;
        clientKey = null;
        ck = await myKey();
        sealed = await ask();
      }
      if (!serverKey) await loadServerKey();
      try {
        return await openSealed(subtle, serverKey.key, ck.pair.privateKey, code, sealed);
      } catch (_) {
        // the server key may have changed since this page loaded (SEAL-8)
        await loadServerKey();
        return await openSealed(subtle, serverKey.key, ck.pair.privateKey, code, sealed);
      }
    }

    // the private values this player may see right now
    function wanted(g) {
      const want = [];
      if (watching || !g || g.me === null) return want;
      if (g.status === "peeking" && !g.seats[g.me].ready) want.push([`o:${g.id}`, { what: "opening" }]);
      if (myTurn(g) && g.stage.kind === "drawn") want.push([`d:${g.id}:${g.turn_seq}`, { what: "drawn" }]);
      for (const r of g.reveals || []) {
        if (live(r.until)) want.push([`p:${r.id}`, { what: "peek", id: r.id }, r.until]);
      }
      return want;
    }

    // drop values no longer shown and fetch newly allowed ones
    function syncSecrets(g) {
      const want = wanted(g);
      const keys = new Set(want.map((w) => w[0]));
      for (const k of [...secrets.keys()]) if (!keys.has(k)) secrets.delete(k);
      for (const [k, body, until] of want) {
        if (asked.has(k)) continue;
        asked.add(k);
        reveal(body)
          .then((value) => {
            if (closed) return;
            if (wanted(game()).some((w) => w[0] === k)) secrets.set(k, Object.assign({ until }, value));
            render();
          })
          .catch((err) => {
            if (closed) return;
            // a peek is revealed once; anything else may be asked for again
            if (!k.startsWith("p:")) asked.delete(k);
            if (err.code !== "already_revealed") toast(err.message || "could not reveal the card");
          });
      }
    }

    function visibleValue(g, seat, slot) {
      const s = g.seats[seat].slots[slot];
      if (!s) return null;
      if (s.v !== undefined) return s.v; // public, once scored
      for (const sec of secrets.values()) {
        const c = (sec.cards || []).find((c) => c.seat === seat && c.slot === slot);
        if (c) return c.v;
      }
      return null;
    }
    function drawnCard(g) {
      const sec = secrets.get(`d:${g.id}:${g.turn_seq}`);
      return sec ? sec.card : null;
    }
    // the card in hand mid turn: drawn (private) or taken (public)
    function heldName(g) {
      const card = g.stage.kind === "taken" ? g.stage.card : drawnCard(g);
      return card === null ? "the drawn card" : `the ${card}`;
    }
    function shownPeeks() {
      return [...secrets.keys()].filter((k) => k.startsWith("p:"));
    }

    function toast(msg) {
      const t = $("toast");
      t.textContent = msg;
      t.hidden = false;
      win.clearTimeout(toast.timer);
      toast.timer = win.setTimeout(() => (t.hidden = true), 3000);
    }

    async function api(method, url, body) {
      const resp = await win.fetch(url, {
        method,
        headers: body ? { "content-type": "application/json" } : {},
        body: body ? JSON.stringify(body) : undefined,
        credentials: "same-origin",
      });
      const data = await resp.json().catch(() => ({}));
      if (!resp.ok) throw Object.assign(new Error(data.message || "request failed"), { code: data.code });
      return data;
    }

    function go(url) {
      win.location.href = url;
    }

    // write markup only when it changed, so buttons a tap is landing on are
    // not swapped out from under it; true when it was written
    function paint(el, html) {
      if (painted.get(el.id) === html) return false;
      painted.set(el.id, html);
      el.innerHTML = html;
      return true;
    }

    // ---------------------------------------------------------------- effects

    function loadSound() {
      try {
        return win.localStorage.getItem(SOUND_KEY) !== "off";
      } catch (_) {
        return true;
      }
    }
    function setSound(on) {
      sound = on;
      try {
        win.localStorage.setItem(SOUND_KEY, on ? "on" : "off");
      } catch (_) {
        // sound still follows the button for this page
      }
      renderSound();
      unlock();
    }
    function renderSound() {
      $("sound").textContent = sound ? "sound on" : "sound off";
      $("sound").setAttribute("aria-pressed", String(sound));
    }
    // browsers only start audio after a gesture, so the context is made on
    // the first tap or key press (UI-27)
    function unlock() {
      const AC = win.AudioContext || win.webkitAudioContext;
      if (!sound || !AC) return;
      if (!audio) audio = new AC();
      if (audio.state === "suspended") audio.resume();
    }
    function play(name, delay) {
      if (sound && audio) SOUNDS[name](audio, audio.currentTime + delay);
    }

    // `shown` is a value the slot shows face up while marked
    function mark(key, cls, shown = null, ms = FX_MS) {
      fx.set(key, { cls, at: Date.now(), shown, ms });
    }
    function fxShown(key) {
      const f = fx.get(key);
      return f ? f.shown : null;
    }
    function fxClass(key) {
      const f = fx.get(key);
      return f ? ` ${f.cls}` : "";
    }
    function fly(from, to, html) {
      flights.push({ from, to, html });
    }
    // a slot "s:<seat>:<slot>", a hand "h:<seat>", or a pile
    function spot(key) {
      const [kind, a, b] = key.split(":");
      if (kind === "h") return doc.querySelector(`#table .hand[data-hand="${a}"]`);
      if (kind === "s") return doc.querySelector(`#table [data-seat="${a}"][data-slot="${b}"]`);
      return $(key);
    }
    // each flight is a card-sized ghost over the target, started at the
    // source's offset and animated home by css
    function launch() {
      for (const f of flights) {
        const a = spot(f.from);
        const b = spot(f.to);
        if (!a || !b) continue;
        const from = a.getBoundingClientRect();
        const to = b.getBoundingClientRect();
        const size = f.from.startsWith("h:") ? to : from;
        const el = doc.createElement("div");
        el.className = "ghost";
        el.innerHTML = f.html;
        el.style.left = `${to.left + (to.width - size.width) / 2}px`;
        el.style.top = `${to.top + (to.height - size.height) / 2}px`;
        el.style.width = `${size.width}px`;
        el.style.height = `${size.height}px`;
        el.style.setProperty("--dx", `${from.left + from.width / 2 - (to.left + to.width / 2)}px`);
        el.style.setProperty("--dy", `${from.top + from.height / 2 - (to.top + to.height / 2)}px`);
        $("fx").append(el);
        win.setTimeout(() => el.remove(), FLY_MS);
      }
      flights = [];
    }

    // marks, flights, and the sound for one new event (UI-24, UI-27); `at`
    // staggers the sounds of several events arriving in one view
    function effect(e, g, at) {
      const p = e.payload || {};
      const seat = g.seats.findIndex((s) => s.player === e.player);
      const slot = (s, n) => `s:${s}:${n}`;
      switch (e.kind) {
        case "start":
          return play("shuffle", at);
        case "draw":
          mark("deck", "fx-pulse");
          fly("deck", `h:${seat}`, back());
          return play("slide", at);
        case "take":
          mark("discard", "fx-pulse");
          fly("discard", `h:${seat}`, face(p.value));
          return play("slide", at);
        case "swap":
          mark(slot(p.seat, p.slot), "fx-flip");
          fly(slot(p.seat, p.slot), "discard", face(p.discarded));
          return play("flip", at);
        case "discard":
          mark("discard", "fx-land");
          fly(`h:${seat}`, "discard", face(p.value));
          return play("flip", at);
        case "peek":
          mark(slot(p.seat, p.slot), "fx-peek");
          return play("peek", at);
        case "blind_swap":
        case "look_swap": {
          const a = slot(p.seat, p.slot);
          const b = slot(p.target_seat, p.target_slot);
          mark(a, "fx-swap");
          mark(b, "fx-swap");
          fly(a, b, back());
          fly(b, a, back());
          return play("swap", at);
        }
        case "match": {
          const target = slot(p.seat, p.slot);
          if (!p.ok) {
            // a room that shows misses names the card's value (SET-13)
            if (typeof p.value === "number") mark(target, "fx-miss", p.value, MISS_SHOW_MS);
            else mark(target, "fx-miss");
            if (p.penalty) {
              const added = slot(seat, g.seats[seat].slots.length - 1);
              mark(added, "fx-flip");
              fly("deck", added, back());
            }
            return play("miss", at);
          }
          mark("discard", "fx-land");
          fly(target, "discard", face(p.value));
          if (typeof p.give_slot === "number") {
            mark(target, "fx-flip");
            fly(slot(seat, p.give_slot), target, back());
          }
          return play("match", at);
        }
        case "komino":
          mark("status", "fx-flash");
          return play("komino", at);
        case "scored":
          g.seats.forEach((s, i) => s.slots.forEach((c, n) => c && mark(slot(i, n), "fx-flip")));
          return play("shuffle", at);
      }
    }

    // whether another player's event touched one of your cards (UI-26)
    function aboutMe(e, g) {
      const p = e.payload || {};
      if (g.me === null) return false;
      if (e.kind === "blind_swap" || e.kind === "look_swap") return p.target_seat === g.me;
      return (e.kind === "peek" || e.kind === "match") && p.seat === g.me;
    }

    function caption(lines) {
      const c = $("caption");
      c.innerHTML = lines.map((l) => `<div${l.mine ? ` class="about-you"` : ""}>${esc(l.text)}</div>`).join("");
      c.hidden = false;
      win.clearTimeout(caption.timer);
      caption.timer = win.setTimeout(() => (c.hidden = true), CAPTION_MS);
    }

    // effects for the events newer than the last one seen in this game; the
    // first view only records where it starts (UI-23)
    function playEvents(before, g) {
      if (!g) return;
      const events = view.events || [];
      const last = seenEvent;
      seenEvent = { game: g.id, id: events.length ? events[0].id : 0 };
      let at = 0;
      if (last) {
        const fresh = events.filter((e) => last.game !== g.id || e.id > last.id).slice(0, 6).reverse();
        const lines = [];
        for (const e of fresh) {
          effect(e, g, at);
          at += 0.25;
          if (e.player !== me && e.kind !== "ready") lines.push({ text: describe(e, g), mine: aboutMe(e, g) });
        }
        if (lines.length) caption(lines);
      }
      // a chime when your turn starts
      if (before && before.id === g.id && myTurn(g) && !(inPlay(before) && before.turn === g.turn)) play("turn", at);
    }

    // ---------------------------------------------------------------- selection

    function setSel(next, text) {
      sel = next;
      if (text) showConfirm(text);
      render();
    }
    function hideConfirm() {
      $("confirm").hidden = true;
    }
    function clearSel() {
      sel = null;
      matchMode = false;
      hideConfirm();
      render();
    }
    function showConfirm(text) {
      $("confirm-text").textContent = text;
      $("confirm").hidden = false;
      $("confirm-ok").focus();
    }
    function confirmAction(action, text, key) {
      const g = game();
      if (g && TURN_ACTIONS.includes(action.type)) action = Object.assign({ turn_seq: g.turn_seq }, action);
      setSel({ key, action }, text);
    }

    function send(action) {
      if (!ws || ws.readyState !== win.WebSocket.OPEN) {
        toast("reconnecting, try again in a moment");
        return;
      }
      const msg = Object.assign({ ref: ++refCounter }, action);
      if (action.type === "match") {
        // the server bounds this by what it saw, so it only helps an honest page
        if (shown && shown.seq === action.seq) msg.reaction_ms = Math.max(0, Math.round(clock() - shown.at));
        claiming = { ref: msg.ref, seq: action.seq, seat: action.seat, slot: action.slot };
      }
      ws.send(JSON.stringify(msg));
    }

    function onResult(data) {
      if (claiming && data.ref === claiming.ref) {
        claiming = null;
        render();
      }
      if (!data.ok) toast(data.code === "too_late" ? "too late, someone matched first"
        : data.code === "stale" ? "the turn moved on; check the table and choose again" : data.message);
    }

    function onSlot(seat, slot) {
      const g = game();
      if (watching || !g || g.me === null) return;
      const mine = seat === g.me;
      const st = g.stage;

      // a match waiting for the card to give away
      if (sel && sel.giving) {
        if (!mine || !filled(g, seat, slot)) return toast("tap one of your own cards to give");
        const a = Object.assign({}, sel.action, { give_slot: slot });
        return confirmAction(a, `match ${seatOwner(g, a.seat)} card ${a.slot + 1} with the ${g.discard_top}, giving your card ${slot + 1}?`, sel.key);
      }
      // the second half of a blind swap
      if (sel && sel.blind) {
        if (mine) return setSel({ key: sel.key, blind: true, slot });
        return confirmAction({ type: "blind_swap", slot: sel.slot, seat, target_slot: slot },
          `blind swap your card ${sel.slot + 1} with ${seatOwner(g, seat)} card ${slot + 1}?`, sel.key);
      }

      if (!matchMode && myTurn(g)) {
        const k = turnKey(g);
        if ((st.kind === "drawn" || st.kind === "taken") && mine) {
          return confirmAction({ type: "swap", slot }, `swap ${heldName(g)} into your card ${slot + 1}?`, k);
        }
        if (st.kind === "special") {
          if (st.mv === "peek_own" && mine) return confirmAction({ type: "peek", seat, slot }, `peek at your card ${slot + 1}?`, k);
          if (st.mv === "peek_other" && !mine) return confirmAction({ type: "peek", seat, slot }, `peek at ${seatOwner(g, seat)} card ${slot + 1}?`, k);
          if (st.mv === "look_swap") return confirmAction({ type: "peek", seat, slot }, `look at ${seatOwner(g, seat)} card ${slot + 1}?`, k);
          if (st.mv === "blind_swap" && mine) return setSel({ key: k, blind: true, slot });
          return toast("that card can't be used for this move");
        }
        if (st.kind === "looked" && mine) {
          return confirmAction({ type: "look_swap", slot }, `swap your card ${slot + 1} with ${seatOwner(g, st.seat)} card ${st.slot + 1}?`, k);
        }
      }

      if (canMatch(g)) {
        const k = matchKey(g);
        const a = { type: "match", seq: g.discard_seq, seat, slot };
        if (!mine) {
          if (!g.seats[g.me].slots.some((s) => s !== null)) return toast("you have no card to give");
          setSel({ key: k, giving: true, action: a });
          return toast("now tap one of your cards to give");
        }
        return confirmAction(a, `match your card ${slot + 1} with the ${g.discard_top}?`, k);
      }
    }

    // ---------------------------------------------------------------- render

    function isChosen(g, seat, slot) {
      if (!sel) return false;
      if (sel.blind) return seat === g.me && sel.slot === slot;
      const a = sel.action;
      switch (a.type) {
        case "swap":
        case "look_swap":
          return seat === g.me && a.slot === slot;
        case "peek":
          return a.seat === seat && a.slot === slot;
        case "blind_swap":
          return (seat === g.me && a.slot === slot) || (a.seat === seat && a.target_slot === slot);
        case "match":
          return (a.seat === seat && a.slot === slot) || (seat === g.me && a.give_slot === slot);
      }
      return false;
    }

    // own reveals light up while face up; anyone else's peek lights the slot
    // for everyone else until the same deadline, without the value
    function isPeeked(g, seat, slot, v) {
      const at = (r) => r.seat === seat && r.slot === slot;
      if ((g.reveals || []).some(at)) return v !== null;
      const p = (g.peeked || []).find(at);
      return Boolean(p) && live(p.until);
    }

    // someone else is looking at this card right now (UI-25)
    function othersPeek(g, seat, slot) {
      const at = (r) => r.seat === seat && r.slot === slot;
      if ((g.reveals || []).some(at)) return false;
      const p = (g.peeked || []).find(at);
      return Boolean(p) && live(p.until);
    }

    // `pos` places the card in its hand's grid; every card shows its slot
    // number, the one the log and confirms use (UI-31)
    function slotButton(g, seat, slot, pos) {
      const s = g.seats[seat].slots[slot];
      const fxc = fxClass(`s:${seat}:${slot}`);
      const no = `<span class="slot-no">${slot + 1}</span>`;
      if (s === null) return `<span class="card empty${fxc}"${pos} data-seat="${seat}" data-slot="${slot}" aria-label="empty slot">${no}</span>`;
      const own = visibleValue(g, seat, slot);
      const missed = own === null ? fxShown(`s:${seat}:${slot}`) : null;
      const v = own === null ? missed : own;
      const chosen = isChosen(g, seat, slot);
      const peeked = isPeeked(g, seat, slot, v);
      const watched = othersPeek(g, seat, slot);
      const pending = Boolean(claiming) && claiming.seat === seat && claiming.slot === slot;
      let label = v === null ? `${seatOwner(g, seat)} card ${slot + 1}, face down` : `${seatOwner(g, seat)} card ${slot + 1}: ${cardLabel(v)}`;
      if (missed !== null) label += ", missed match";
      if (watched) label += ", being peeked at";
      if (pending) label += ", matching";
      const lock = g.seats[seat].locked ? `<span class="lock">locked</span>` : "";
      const badge = watched ? `<span class="eye-badge"><svg viewBox="0 0 34 16" aria-hidden="true">${eye(17, 8, "#2f6fd6")}</svg></span>` : "";
      return `<button class="card${v === null ? " down" : ""}${chosen ? " sel" : ""}${pending ? " claiming" : ""}${peeked ? " peeked" : ""}${fxc}"${pos} data-seat="${seat}" data-slot="${slot}" aria-label="${esc(label)}">` +
        (v === null ? back() : face(v)) + no + lock + badge + `</button>`;
    }

    // the near row holds slots 1 to floor(n / 2), the rows behind it the
    // rest, penalty slots farthest back (SET-5). Owners see the near row at
    // the bottom; a seated viewer sees other players across the table,
    // turned 180 degrees: near row on top, slot 1 top right (UI-30).
    function slotPos(g, seat, slot, count) {
      const cols = columns(g);
      const near = (g.hand_size || 4) - cols;
      // row back from the owner's near edge, and column from their left
      const [back, col] = slot < near ? [0, slot] : [1 + Math.floor((slot - near) / cols), (slot - near) % cols];
      if (seat !== g.me && g.me !== null) return ` style="grid-area: ${back + 1} / ${cols - col}"`;
      const rows = 1 + Math.ceil(Math.max(0, count - near) / cols);
      return ` style="grid-area: ${rows - back} / ${col + 1}"`;
    }

    function hand(g, seat, mine) {
      const s = g.seats[seat];
      const p = (view.members || []).find((m) => m.id === s.player) || {};
      let tag = "";
      if (s.forfeited) tag = " (out)";
      else if (g.status === "peeking") tag = s.ready ? " (ready)" : " (peeking)";
      if (s.score !== null && s.score !== undefined) tag = ` <span class="score${s.won ? " won" : ""}">${s.score}${s.won ? " won" : ""}</span>`;
      const slots = s.slots.map((_, i) => slotButton(g, seat, i, slotPos(g, seat, i, s.slots.length))).join("");
      const cols = columns(g);
      return `<div class="hand${mine ? " mine" : ""}${g.turn === seat && inPlay(g) ? " turn" : ""}" data-hand="${seat}">` +
        `<div class="who"><span class="dot${p.present ? " on" : ""}"></span>${esc(mine ? "you" : p.name || "?")}${tag}</div>` +
        `<div class="slots${cols > 3 ? " wide" : ""}" style="--cols: ${cols}">${slots}</div></div>`;
    }

    // the handlers read the current view, since an unchanged table keeps
    // the buttons (and their handlers) from an earlier render
    function renderTable(g) {
      const t = $("table");
      if (!g) {
        paint(t, `<p>No game yet. The host starts one once at least two players are here.</p>`);
        return;
      }
      const others = g.seats.map((_, i) => i).filter((i) => i !== g.me).map((i) => hand(g, i, false)).join("");
      const top = g.discard_top;
      const card = g.stage.kind === "drawn" && myTurn(g) ? drawnCard(g) : null;
      const drawn = card === null ? ""
        : `<div class="pile"><span class="card" id="drawn">${face(card)}</span><span>drawn</span></div>`;
      const fresh = paint(t,
        `<div class="opponents">${others}</div>` +
        `<div class="center">` +
        `<div class="pile"><button class="card${fxClass("deck")}" id="deck" aria-label="draw pile, ${g.deck_count} cards">${back()}</button><span>${g.deck_count} left</span></div>` +
        `<div class="pile"><button class="card${fxClass("discard")}" id="discard" aria-label="discard pile${top === null ? ", empty" : ", " + cardLabel(top)}">${top === null ? "" : face(top)}</button><span>${g.matchable ? "matchable" : "discard"}</span></div>` +
        drawn + `</div>` +
        (g.me !== null ? hand(g, g.me, true) : ""));
      if (!fresh) return;
      t.querySelectorAll("button[data-seat]").forEach((b) =>
        b.addEventListener("click", () => onSlot(Number(b.dataset.seat), Number(b.dataset.slot))));
      $("deck").addEventListener("click", () => {
        const g = game();
        if (myTurn(g) && g.stage.kind === "start") confirmAction({ type: "draw" }, "draw from the deck?", turnKey(g));
      });
      $("discard").addEventListener("click", () => {
        const g = game();
        if (myTurn(g) && g.stage.kind === "start" && g.discard_top !== null) {
          confirmAction({ type: "take" }, `take the ${g.discard_top} from the discard pile?`, turnKey(g));
        }
      });
      // a mark redrawn mid animation picks up where it was (UI-24)
      for (const [key, f] of fx) {
        const el = key === "status" ? null : spot(key);
        if (el) el.style.animationDelay = `-${Date.now() - f.at}ms`;
      }
    }

    // deadlines are server times, read against the corrected clock
    function secondsLeft(deadline) {
      return deadline ? Math.max(0, Math.ceil((deadline - Date.now() - skew) / 1000)) : null;
    }

    function renderStatus(g) {
      const el = $("status");
      if (!g) return (el.textContent = watching ? "watching. no game yet." : "");
      const seated = g.me !== null;
      const turnName = g.turn === g.me ? "your" : `${nameOf(g.seats[g.turn].player)}'s`;
      let text = "";
      if (g.status === "peeking") {
        const wait = `play starts in ${secondsLeft(g.ready_deadline)}s or when everyone is ready.`;
        const near = (g.hand_size || 4) - columns(g);
        const row = near === 2 ? "bottom two cards" : `bottom ${near} cards`;
        if (!seated) text = `players are memorizing their ${row}. ${wait}`;
        else if (g.seats[g.me].ready) text = `waiting for the others. ${wait}`;
        else text = `memorize your ${row}. ${wait}`;
      } else if (inPlay(g)) {
        text = `${turnName} turn`;
        if (g.turn_deadline) text += ` (${secondsLeft(g.turn_deadline)}s left)`;
        if (g.away_deadline) text += ` (away, skipping in ${secondsLeft(g.away_deadline)}s)`;
        if (myTurn(g)) text += hint(g);
      } else if (g.status === "scoring") {
        text = `scoring in ${secondsLeft(g.score_at)}s, last chance to match`;
      } else {
        const w = g.seats.filter((s) => s.won).map((s) => (s.player === me && seated ? "you" : nameOf(s.player)));
        text = w.length ? `game over. winner: ${w.join(", ")}` : "game over";
      }
      let banner = g.caller !== null && g.status !== "scored"
        ? `<span class="komino">KOMINO called by ${esc(g.caller === g.me ? "you" : nameOf(g.seats[g.caller].player))}, ${g.final_remaining.length} turn(s) left. </span>` : "";
      if (g.calling) banner = `<span class="komino">you called KOMINO; it takes effect when your turn ends. </span>`;
      el.innerHTML = banner + esc(text);
      el.classList.toggle("fx-flash", fx.has("status"));
    }

    function hint(g) {
      const st = g.stage;
      if (sel && sel.blind) return ": now tap another player's card";
      if (st.kind === "start") return ": draw from the deck or take the discard";
      if (st.kind === "drawn") return ": tap one of your cards to swap, or discard";
      if (st.kind === "taken") return ": tap one of your cards to swap";
      if (st.kind === "earned") return `: match the discard, use ${MOVE_INFO[st.mv].label}, or end your turn`;
      if (st.kind === "special") {
        return {
          peek_own: ": tap one of your cards to peek",
          peek_other: ": tap another player's card to peek",
          blind_swap: ": tap your card, then another player's card",
          look_swap: ": tap any card to look at it",
        }[st.mv] + ", or skip";
      }
      if (st.kind === "looked") return ": tap your card to swap with it, or keep yours";
      return "";
    }

    function button(label, onClick, opts) {
      const b = doc.createElement("button");
      b.textContent = label;
      if (opts && opts.cls) b.className = opts.cls;
      if (opts && opts.disabled) b.disabled = true;
      if (opts && opts.title) b.title = opts.title;
      b.addEventListener("click", onClick);
      return b;
    }

    // the buttons are rebuilt only when their labels or states change, and
    // each press runs the handler from the latest render
    function renderControls(g) {
      const specs = [];
      const add = (label, onClick, opts) => specs.push({ label, onClick, opts: opts || {} });
      if (!watching && g && g.me !== null) controlSpecs(g, add);
      controlHandlers = specs.map((s) => s.onClick);
      const sig = JSON.stringify(specs.map((s) => [s.label, s.opts]));
      if (painted.get("controls") === sig) return;
      painted.set("controls", sig);
      const c = $("controls");
      c.innerHTML = "";
      specs.forEach((s, i) => c.append(button(s.label, () => controlHandlers[i](), s.opts)));
    }

    function controlSpecs(g, add) {
      const seat = g.seats[g.me];
      const k = turnKey(g);
      if (g.status === "peeking" && !seat.ready) {
        add("ready", () => confirmAction({ type: "ready" }, "done memorizing your cards?", k), { cls: "primary" });
      }
      if (myTurn(g)) {
        const st = g.stage;
        if (st.kind === "start") {
          add("draw", () => confirmAction({ type: "draw" }, "draw from the deck?", k));
          if (g.discard_top !== null) add(`take ${g.discard_top}`, () => confirmAction({ type: "take" }, `take the ${g.discard_top} from the discard pile?`, k));
        }
        if (st.kind === "drawn") {
          const card = drawnCard(g);
          const move = card !== null && MOVES[card] ? `? you can match first, then use ${MOVE_INFO[MOVES[card]].label}` : "?";
          add(card === null ? "discard" : `discard ${card}`,
            () => confirmAction({ type: "discard" }, `discard ${heldName(g)}${move}`, k));
        }
        if (st.kind === "earned") {
          const label = MOVE_INFO[st.mv].label;
          add(`use ${label}`, () => confirmAction({ type: "use_special" }, `use ${label} now?`, k), { cls: "primary" });
          add("end turn", () => confirmAction({ type: "skip" }, `end your turn without using ${label}?`, k));
        }
        if (st.kind === "special") add("skip move", () => confirmAction({ type: "skip" }, "skip the special move?", k));
        if (st.kind === "looked") add("keep my cards", () => confirmAction({ type: "look_swap", slot: null }, "keep your cards and end the turn?", k));
      }
      // while a move waits to be used, taps on cards already match
      if (canMatch(g) && myTurn(g) && !["start", "earned"].includes(g.stage.kind)) {
        add(matchMode ? "cancel match" : `match ${g.discard_top}`, () => {
          matchMode = !matchMode;
          sel = null;
          hideConfirm();
          if (matchMode) toast(`tap a card you think is a ${g.discard_top}`);
          render();
        });
      }
      // a live peek the server still holds shows the button too, so a peek
      // whose value was lost to a reload can still be ended (SET-12)
      const held = (g.reveals || []).some((r) => live(r.until));
      if (held || shownPeeks().length) {
        add("hide card", () => {
          // forget the values; a peek can't be fetched again
          for (const k of shownPeeks()) secrets.delete(k);
          if (held) send({ type: "hide" });
          render();
        });
      }
      // komino can be called any time in your own turn; mid turn it lands
      // when the turn ends (RULE-22)
      const why = g.status !== "playing" ? "only during play" : !myTurn(g) ? "only on your turn"
        : g.calling ? "called; it takes effect when your turn ends" : "everyone must take a turn first";
      const ask = g.stage.kind === "start" ? "call KOMINO? everyone else gets one more turn."
        : "call KOMINO? it takes effect when this turn ends, then everyone else gets one more turn.";
      add(g.calling ? "KOMINO called" : "KOMINO", () => confirmAction({ type: "komino" }, ask, k),
        { cls: "komino-btn", disabled: !g.can_call, title: g.can_call ? "end the round" : why });
    }

    function renderMembers() {
      const ul = $("members");
      const host = view.room.host;
      const amHost = !watching && host === me;
      const sig = JSON.stringify([host, amHost, view.members]);
      if (painted.get("members") === sig) return;
      painted.set("members", sig);
      ul.innerHTML = "";
      for (const m of view.members) {
        if (m.left && !m.removed) continue;
        const li = doc.createElement("li");
        const tags = [m.id === host ? "host" : "", m.id === me ? "you" : "", m.removed ? "removed" : ""].filter(Boolean).join(", ");
        li.innerHTML = `<span class="dot${m.present ? " on" : ""}"></span><span>${esc(m.name)}</span><span class="tag">${esc(tags)}</span>`;
        if (amHost && m.id !== me) {
          if (m.removed) {
            li.append(button("unban", () => api("POST", `/komino/api/rooms/${code}/unban`, { player: m.id }).then(applyView).catch((e) => toast(e.message)), { cls: "small" }));
          } else {
            li.append(button("remove", () => {
              if (win.confirm(`remove ${m.name} from the room?`)) {
                api("POST", `/komino/api/rooms/${code}/remove`, { player: m.id }).then(applyView).catch((e) => toast(e.message));
              }
            }, { cls: "small" }));
          }
        }
        ul.append(li);
      }
    }

    function describe(e, g) {
      const p = e.payload || {};
      const who = e.player ? (e.player === me ? "you" : nameOf(e.player)) : "";
      const owner = (seat) => (g && g.seats[seat] ? (g.seats[seat].player === me ? "your" : `${nameOf(g.seats[seat].player)}'s`) : "a");
      switch (e.kind) {
        case "start": return `${who} started a game`;
        case "ready": return `${who} ${e.player === me ? "are" : "is"} ready`;
        case "play": return "play begins";
        case "draw": return `${who} drew a card`;
        case "take": return `${who} took the ${p.value}`;
        case "swap": return `${who} swapped into card ${p.slot + 1}, discarding ${p.discarded}`;
        case "discard": return `${who} discarded ${p.value}`;
        case "peek": return `${who} peeked at ${owner(p.seat)} card ${p.slot + 1}`;
        case "blind_swap": return `${who} blind swapped with ${owner(p.target_seat)} card ${p.target_slot + 1}`;
        case "look_swap": return `${who} swapped with ${owner(p.target_seat)} card ${p.target_slot + 1}`;
        case "skip": return `${who} skipped the move`;
        case "match": {
          if (p.ok) return `${who} matched ${owner(p.seat)} ${p.value}`;
          const value = typeof p.value === "number" ? `, a ${p.value},` : "";
          return `${who} missed a match on ${owner(p.seat)} card ${p.slot + 1}${value} and took a penalty`;
        }
        case "komino": return `${who} called KOMINO`;
        case "forfeit": return `${who} left the game`;
        case "away_skip": return `${who} ${e.player === me ? "were" : "was"} away; turn skipped`;
        case "timeout_skip": return `${who} ran out of time; turn skipped`;
        case "scored": return `game over: ${(p.winners || []).map((w) => (w === me ? "you" : nameOf(w))).join(", ") || "no winner"}`;
      }
      return e.kind;
    }

    function renderLog(g) {
      paint($("log"), (view.events || []).map((e) => `<li>${esc(describe(e, g))}</li>`).join(""));
    }

    // e.g. "6 cards, 60s turns, 30s away grace, peeks until hidden" (SET-4)
    function settingsText(s) {
      if (!s) return "";
      return [
        `${s.hand_size} cards`,
        s.turn_limit_secs ? `${s.turn_limit_secs}s turns` : "no turn limit",
        `${s.away_grace_secs}s away grace`,
        s.reveal_secs ? `${s.reveal_secs}s peeks` : "peeks until hidden",
        s.show_misses === false ? "misses hidden" : "misses shown",
      ].join(", ");
    }

    function renderStats() {
      const cols = [["games_played", "games"], ["wins", "wins"], ["komino_calls", "calls"], ["komino_wins", "call wins"],
        ["matches", "matches"], ["failed_matches", "missed"], ["special_moves", "specials"], ["cards_interacted", "cards"], ["forfeits", "forfeits"]];
      const rows = view.stats || [];
      if (!rows.length) return paint($("stats"), "<tr><td>no games yet</td></tr>");
      const gone = new Set(view.members.filter((m) => m.left || m.removed).map((m) => m.id));
      paint($("stats"), `<tr><th>player</th>${cols.map((c) => `<th>${c[1]}</th>`).join("")}</tr>` +
        rows.map((r) => `<tr><td>${esc(r.name)}${gone.has(r.player) ? " (gone)" : ""}</td>${cols.map((c) => `<td>${r[c[0]]}</td>`).join("")}</tr>`).join(""));
    }

    function render() {
      if (!view) return;
      const g = game();
      // every render (each view and each second) drops expired values
      syncSecrets(g);
      for (const [k, f] of fx) if (Date.now() - f.at >= f.ms) fx.delete(k);
      $("code").textContent = view.room.code;
      $("settings").textContent = settingsText(view.room.settings);
      const amHost = !watching && view.room.host === me;
      const canStart = amHost && (!g || g.status === "scored");
      $("start").hidden = !canStart;
      $("start").textContent = g ? "next game" : "start game";
      const n = view.observers || 0;
      $("observers").hidden = n === 0;
      $("observers").textContent = `${n} watching`;
      renderStatus(g);
      renderTable(g);
      renderControls(g);
      renderMembers();
      renderLog(g);
      renderStats();
    }

    function applyView(v) {
      const before = game();
      view = v;
      if (typeof v.server_now === "number") skew = v.server_now - Date.now();
      const g = game();
      if (g && (!shown || shown.game !== g.id || shown.seq !== g.discard_seq)) {
        shown = { game: g.id, seq: g.discard_seq, at: clock() };
      }
      // a pending match ends with its result, or once its discard is gone
      if (claiming && (!g || g.discard_seq !== claiming.seq)) claiming = null;
      // a pending action that no longer applies is dropped, with the reason
      if (sel && g) {
        const now = sel.key.startsWith("m:") ? matchKey(g) : turnKey(g);
        if (now !== sel.key) {
          sel = null;
          matchMode = false;
          hideConfirm();
          toast(before && before.discard_seq !== g.discard_seq ? "too late: the discard changed" : "that move is no longer available");
        }
      }
      playEvents(before, g);
      render();
      launch();
    }

    // ---------------------------------------------------------------- socket

    function connect() {
      const proto = win.location.protocol === "https:" ? "wss" : "ws";
      ws = new win.WebSocket(`${proto}://${win.location.host}/komino/r/${code}${watching ? "/watch" : ""}/ws`);
      ws.onopen = () => {
        backoff = 500;
        $("status").classList.remove("offline");
      };
      ws.onmessage = (msg) => {
        const data = JSON.parse(msg.data);
        if (data.type === "view") applyView(data.view);
        else if (data.type === "removed") gone("removed", "The host removed you from this room.");
        else if (data.type === "full") gone("room is full", `${data.message}. Try again later.`);
        else if (data.type === "result") onResult(data);
        else if (data.type === "error") toast(data.message);
      };
      ws.onclose = () => {
        if (closed) return;
        $("status").textContent = "reconnecting...";
        $("status").classList.add("offline");
        win.setTimeout(connect, backoff);
        backoff = Math.min(backoff * 2, MAX_BACKOFF);
      };
    }

    function disconnect() {
      closed = true;
      if (ws) {
        ws.onclose = null;
        ws.close();
      }
    }

    function gone(title, text) {
      $("lobby").hidden = true;
      $("room").hidden = true;
      $("gone").hidden = false;
      $("gone-title").textContent = title;
      $("gone-text").textContent = text;
      disconnect();
    }

    // ---------------------------------------------------------------- wiring

    function wire() {
      $("confirm-cancel").addEventListener("click", clearSel);
      $("confirm-ok").addEventListener("click", () => {
        if (sel && sel.action) send(sel.action);
        clearSel();
      });
      // a tap outside the dialog is a cancel
      $("confirm").addEventListener("click", (e) => {
        if (e.target === $("confirm")) clearSel();
      });
      doc.addEventListener("keydown", (e) => {
        if (e.key === "Escape") clearSel();
      });
      doc.addEventListener("pointerdown", unlock, true);
      doc.addEventListener("keydown", unlock, true);
      renderSound();
      $("sound").addEventListener("click", () => setSound(!sound));

      $("name").addEventListener("change", async (e) => {
        try {
          const r = await api("POST", "/komino/api/me", { name: e.target.value });
          e.target.value = r.name;
        } catch (err) {
          toast(err.message);
        }
      });

      $("create").addEventListener("click", async () => {
        // an empty choice is off (turn limit) or until hidden (peek time)
        const secs = (id) => ($(id).value === "" ? null : Number($(id).value));
        const settings = {
          hand_size: Number($("set-hand").value),
          turn_limit_secs: secs("set-turn"),
          away_grace_secs: Number($("set-away").value),
          reveal_secs: secs("set-reveal"),
          show_misses: $("set-misses").value === "shown",
        };
        try {
          const v = await api("POST", "/komino/api/rooms", settings);
          go(`/komino/r/${v.room.code}`);
        } catch (err) {
          toast(err.message);
        }
      });
      $("join-form").addEventListener("submit", (e) => {
        e.preventDefault();
        const c = $("join-code").value.trim().toUpperCase();
        if (c) go(`/komino/r/${c}`);
      });
      const copy = (url) => async () => {
        try {
          await win.navigator.clipboard.writeText(url());
          toast("link copied");
        } catch (_) {
          toast(url());
        }
      };
      $("copy").addEventListener("click", copy(() => view.room.url));
      $("copy-watch").addEventListener("click", copy(() => view.room.watch_url));
      $("start").addEventListener("click", () => send({ type: "start" }));
      $("leave").addEventListener("click", async () => {
        if (!win.confirm("leave this room? if you are in a game you forfeit it.")) return;
        await api("POST", `/komino/api/rooms/${code}/leave`).catch(() => {});
        disconnect();
        go("/komino");
      });
    }

    async function start() {
      wire();
      // countdowns and peek timeouts
      ticker = win.setInterval(() => {
        if (view && game() && game().status !== "scored") render();
      }, 1000);
      try {
        const who = await api("GET", "/komino/api/me");
        me = who.id;
        $("name").value = who.name;
      } catch (err) {
        return toast(err.message);
      }
      if (!code) {
        $("lobby").hidden = false;
        return;
      }
      $("room").hidden = false;
      if (watching) {
        doc.body.classList.add("watching");
        $("join-link").href = `/komino/r/${code}`;
      } else {
        // cached for the page; a failure here is retried at the first reveal
        await loadServerKey().catch(() => {});
      }
      try {
        applyView(watching
          ? await api("GET", `/komino/api/rooms/${code}/watch`)
          : await api("POST", `/komino/api/rooms/${code}/join`));
      } catch (err) {
        if (err.code === "removed") return gone("removed", "The host removed you from this room.");
        return gone("room not found", `There is no room with code ${code}.`);
      }
      connect();
    }

    function stop() {
      win.clearInterval(ticker);
      disconnect();
    }

    return { start, stop };
  }

  if (typeof module === "object" && module.exports) {
    module.exports = { createKomino, face, back, cardLabel, openSealed, importServerKey, b64d, b64e };
  } else {
    createKomino(root).start();
  }
})(typeof window === "undefined" ? globalThis : window);
