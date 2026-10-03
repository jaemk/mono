// komino client. The server owns all state and sends a redacted view; this
// page renders it, turns taps into a select-then-confirm action, and sends
// the confirmed action over the room websocket.
(function () {
  "use strict";

  const $ = (id) => document.getElementById(id);
  const esc = (s) =>
    String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

  const code = (location.pathname.match(/^\/komino\/r\/([A-Za-z0-9]{6})/) || [])[1];
  let view = null;
  let me = null;
  let ws = null;
  let backoff = 500;
  let sel = null; // pending selection: { key, kind, ...fields }
  let matchMode = false;
  const revealSeen = {}; // reveal key -> local time first seen
  const dismissed = new Set();
  let refCounter = 0;

  // ---------------------------------------------------------------- cards

  const MOVES = { 7: "peek_own", 8: "peek_own", 9: "peek_other", 10: "peek_other", 11: "blind_swap", 12: "blind_swap", 13: "look_swap" };
  const MOVE_INFO = {
    peek_own: { color: "#2f6fd6", label: "peek own" },
    peek_other: { color: "#e07a1f", label: "peek other" },
    blind_swap: { color: "#7b4bc9", label: "blind swap" },
    look_swap: { color: "#d0393b", label: "look and swap" },
  };

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

  function face(v) {
    const mv = MOVES[v];
    const ink = mv ? MOVE_INFO[mv].color : "#1f1d1a";
    const bg = mv ? "#fff" : v <= 0 ? "var(--card-low)" : "var(--card)";
    const center = mv
      ? symbol(mv)
      : `<text x="35" y="62" text-anchor="middle" font-size="34" font-weight="700" fill="${ink}">${v}</text>`;
    return `<svg viewBox="0 0 70 100" aria-hidden="true">` +
      `<rect x="1" y="1" width="68" height="98" rx="7" fill="${bg}" stroke="${mv ? ink : "#bdb6a6"}" stroke-width="2"/>` +
      `<text x="8" y="18" font-size="14" font-weight="700" fill="${ink}">${v}</text>` +
      `<text x="62" y="82" font-size="14" font-weight="700" fill="${ink}" text-anchor="end" transform="rotate(180 58 87)">${v}</text>` +
      center + `</svg>`;
  }

  function back() {
    return `<svg viewBox="0 0 70 100" aria-hidden="true">` +
      `<rect x="1" y="1" width="68" height="98" rx="7" fill="var(--back)" stroke="#0003" stroke-width="2"/>` +
      `<rect x="7" y="7" width="56" height="86" rx="4" fill="none" stroke="var(--back-2)" stroke-width="2"/>` +
      `<path d="M35 20 L52 50 L35 80 L18 50 Z" fill="var(--back-2)"/>` +
      `<circle cx="35" cy="50" r="6" fill="var(--back)"/></svg>`;
  }

  function cardLabel(v) {
    const mv = MOVES[v];
    return mv ? `${v}, ${MOVE_INFO[mv].label}` : String(v);
  }

  // ---------------------------------------------------------------- helpers

  function names() {
    const m = {};
    for (const p of (view && view.members) || []) m[p.id] = p.name;
    return m;
  }
  function nameOf(id) {
    return names()[id] || "someone";
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

  function visibleValue(g, seat, slot) {
    const s = g.seats[seat].slots[slot];
    if (!s || s.v === undefined) return null;
    if (g.status === "scored" || g.status === "peeking") return s.v;
    const r = (g.reveals || []).find((r) => r.seat === seat && r.slot === slot);
    if (!r) return null;
    const key = `${g.id}:${seat}:${slot}:${r.until}`;
    if (dismissed.has(key)) return null;
    revealSeen[key] = revealSeen[key] || Date.now();
    return Date.now() - revealSeen[key] < 5000 ? s.v : null;
  }

  function toast(msg) {
    const t = $("toast");
    t.textContent = msg;
    t.hidden = false;
    clearTimeout(toast.timer);
    toast.timer = setTimeout(() => (t.hidden = true), 3000);
  }

  async function api(method, path, body) {
    const resp = await fetch(path, {
      method,
      headers: body ? { "content-type": "application/json" } : {},
      body: body ? JSON.stringify(body) : undefined,
      credentials: "same-origin",
    });
    const data = await resp.json().catch(() => ({}));
    if (!resp.ok) throw Object.assign(new Error(data.message || "request failed"), { code: data.code });
    return data;
  }

  // ---------------------------------------------------------------- selection

  function setSel(next, text) {
    sel = next;
    if (text) showConfirm(text);
    render();
  }
  function clearSel() {
    sel = null;
    matchMode = false;
    $("confirm").hidden = true;
    render();
  }
  function showConfirm(text) {
    $("confirm-text").textContent = text;
    $("confirm").hidden = false;
  }
  // turn actions carry the turn token they were chosen against, so a stale
  // or repeated confirm is refused instead of applied to a moved-on turn
  const TURN_ACTIONS = ["draw", "take", "swap", "discard", "komino", "peek", "blind_swap", "look_swap", "skip"];
  function confirmAction(action, text, key) {
    const g = game();
    if (g && TURN_ACTIONS.includes(action.type)) action = Object.assign({ turn_seq: g.turn_seq }, action);
    setSel({ key, action }, text);
  }

  function send(action) {
    if (!ws || ws.readyState !== WebSocket.OPEN) {
      toast("reconnecting, try again in a moment");
      return;
    }
    ws.send(JSON.stringify(Object.assign({ ref: ++refCounter }, action)));
  }

  function onSlot(seat, slot) {
    const g = game();
    if (!g || g.me === null) return;
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
        return confirmAction({ type: "swap", slot }, `swap the ${st.card} into your card ${slot + 1}?`, k);
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
    if (!a) return false;
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

  function slotButton(g, seat, slot) {
    const s = g.seats[seat].slots[slot];
    if (s === null) return `<span class="card empty" aria-label="empty slot"></span>`;
    const v = visibleValue(g, seat, slot);
    const chosen = isChosen(g, seat, slot);
    const peeked = (g.reveals || []).some((r) => r.seat === seat && r.slot === slot) && v !== null;
    const label = v === null ? `${seatOwner(g, seat)} card ${slot + 1}, face down` : `${seatOwner(g, seat)} card ${slot + 1}: ${cardLabel(v)}`;
    const lock = g.seats[seat].locked ? `<span class="lock">locked</span>` : "";
    return `<button class="card${chosen ? " sel" : ""}${peeked ? " peeked" : ""}" data-seat="${seat}" data-slot="${slot}" aria-label="${esc(label)}">` +
      (v === null ? back() : face(v)) + lock + `</button>`;
  }

  function hand(g, seat, mine) {
    const s = g.seats[seat];
    const p = (view.members || []).find((m) => m.id === s.player) || {};
    let tag = "";
    if (s.forfeited) tag = " (out)";
    else if (g.status === "peeking") tag = s.ready ? " (ready)" : " (peeking)";
    if (s.score !== null && s.score !== undefined) tag = ` <span class="score${s.won ? " won" : ""}">${s.score}${s.won ? " won" : ""}</span>`;
    const slots = s.slots.map((_, i) => slotButton(g, seat, i)).join("");
    return `<div class="hand${mine ? " mine" : ""}${g.turn === seat && inPlay(g) ? " turn" : ""}">` +
      `<div class="who"><span class="dot${p.present ? " on" : ""}"></span>${esc(mine ? "you" : p.name || "?")}${tag}</div>` +
      `<div class="slots">${slots}</div></div>`;
  }

  function renderTable(g) {
    const t = $("table");
    if (!g) {
      t.innerHTML = `<p>No game yet. The host starts one once at least two players are here.</p>`;
      return;
    }
    const others = g.seats.map((_, i) => i).filter((i) => i !== g.me).map((i) => hand(g, i, false)).join("");
    const top = g.discard_top;
    const drawn = g.stage.kind === "drawn" && g.stage.card !== null && g.stage.card !== undefined && myTurn(g)
      ? `<div class="pile"><span class="card">${face(g.stage.card)}</span><span>drawn</span></div>` : "";
    t.innerHTML =
      `<div class="opponents">${others}</div>` +
      `<div class="center">` +
      `<div class="pile"><button class="card" id="deck" aria-label="draw pile, ${g.deck_count} cards">${back()}</button><span>${g.deck_count} left</span></div>` +
      `<div class="pile"><button class="card" id="discard" aria-label="discard pile${top === null ? ", empty" : ", " + cardLabel(top)}">${top === null ? "" : face(top)}</button><span>${g.matchable ? "matchable" : "discard"}</span></div>` +
      drawn + `</div>` +
      (g.me !== null ? hand(g, g.me, true) : "");
    t.querySelectorAll("button[data-seat]").forEach((b) =>
      b.addEventListener("click", () => onSlot(Number(b.dataset.seat), Number(b.dataset.slot))));
    const deck = $("deck");
    if (deck) deck.addEventListener("click", () => {
      if (myTurn(g) && g.stage.kind === "start") confirmAction({ type: "draw" }, "draw from the deck?", turnKey(g));
    });
    const disc = $("discard");
    if (disc) disc.addEventListener("click", () => {
      if (myTurn(g) && g.stage.kind === "start" && top !== null) confirmAction({ type: "take" }, `take the ${top} from the discard pile?`, turnKey(g));
    });
  }

  function secondsLeft(deadline) {
    return deadline ? Math.max(0, Math.ceil((deadline - Date.now()) / 1000)) : null;
  }

  function renderStatus(g) {
    const el = $("status");
    if (!g) return (el.textContent = "");
    const turnName = g.turn === g.me ? "your" : `${nameOf(g.seats[g.turn].player)}'s`;
    let text = "";
    if (g.status === "peeking") {
      text = `memorize your bottom two cards. play starts in ${secondsLeft(g.ready_deadline)}s or when everyone is ready.`;
    } else if (g.status === "playing" || g.status === "final") {
      text = `${turnName} turn`;
      if (g.away_deadline) text += ` (away, skipping in ${secondsLeft(g.away_deadline)}s)`;
      if (myTurn(g)) text += hint(g);
    } else if (g.status === "scoring") {
      text = `scoring in ${secondsLeft(g.score_at)}s, last chance to match`;
    } else {
      const w = g.seats.filter((s) => s.won).map((s) => (s.player === me ? "you" : nameOf(s.player)));
      text = w.length ? `game over. winner: ${w.join(", ")}` : "game over";
    }
    const banner = g.caller !== null && g.status !== "scored"
      ? `<span class="komino">KOMINO called by ${esc(g.caller === g.me ? "you" : nameOf(g.seats[g.caller].player))}, ${g.final_remaining.length} turn(s) left. </span>` : "";
    el.innerHTML = banner + esc(text);
  }

  function hint(g) {
    const st = g.stage;
    if (sel && sel.blind) return ": now tap another player's card";
    if (st.kind === "start") return ": draw from the deck or take the discard";
    if (st.kind === "drawn") return ": tap one of your cards to swap, or discard";
    if (st.kind === "taken") return ": tap one of your cards to swap";
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
    const b = document.createElement("button");
    b.textContent = label;
    if (opts && opts.cls) b.className = opts.cls;
    if (opts && opts.disabled) b.disabled = true;
    if (opts && opts.title) b.title = opts.title;
    b.addEventListener("click", onClick);
    return b;
  }

  function renderControls(g) {
    const c = $("controls");
    c.innerHTML = "";
    if (!g || g.me === null) return;
    const seat = g.seats[g.me];
    const k = turnKey(g);
    if (g.status === "peeking" && !seat.ready) {
      c.append(button("ready", () => confirmAction({ type: "ready" }, "done memorizing your cards?", k), { cls: "primary" }));
    }
    if (myTurn(g)) {
      const st = g.stage;
      if (st.kind === "start") {
        c.append(button("draw", () => confirmAction({ type: "draw" }, "draw from the deck?", k)));
        if (g.discard_top !== null) c.append(button(`take ${g.discard_top}`, () => confirmAction({ type: "take" }, `take the ${g.discard_top} from the discard pile?`, k)));
      }
      if (st.kind === "drawn") c.append(button(`discard ${st.card}`, () => confirmAction({ type: "discard" }, `discard the ${st.card}${MOVES[st.card] ? " and use " + MOVE_INFO[MOVES[st.card]].label : ""}?`, k)));
      if (st.kind === "special") c.append(button("skip move", () => confirmAction({ type: "skip" }, "skip the special move?", k)));
      if (st.kind === "looked") c.append(button("keep my cards", () => confirmAction({ type: "look_swap", slot: null }, "keep your cards and end the turn?", k)));
    }
    if (canMatch(g) && myTurn(g) && g.stage.kind !== "start") {
      c.append(button(matchMode ? "cancel match" : `match ${g.discard_top}`, () => {
        matchMode = !matchMode;
        sel = null;
        $("confirm").hidden = true;
        if (matchMode) toast(`tap a card you think is a ${g.discard_top}`);
        render();
      }));
    }
    const why = g.status !== "playing" ? "only during play" : !myTurn(g) ? "only at the start of your turn"
      : g.stage.kind !== "start" ? "only before drawing" : "everyone must take a turn first";
    c.append(button("KOMINO", () => confirmAction({ type: "komino" }, "call KOMINO? everyone else gets one more turn.", k),
      { cls: "komino-btn", disabled: !g.can_call, title: g.can_call ? "end the round" : why }));
  }

  function renderMembers() {
    const ul = $("members");
    const host = view.room.host;
    const amHost = host === me;
    ul.innerHTML = "";
    for (const m of view.members) {
      if (m.left && !m.removed) continue;
      const li = document.createElement("li");
      const tags = [m.id === host ? "host" : "", m.id === me ? "you" : "", m.removed ? "removed" : ""].filter(Boolean).join(", ");
      li.innerHTML = `<span class="dot${m.present ? " on" : ""}"></span><span>${esc(m.name)}</span><span class="tag">${esc(tags)}</span>`;
      if (amHost && m.id !== me) {
        if (m.removed) {
          li.append(button("unban", () => api("POST", `/komino/api/rooms/${code}/unban`, { player: m.id }).then(applyView).catch((e) => toast(e.message)), { cls: "small" }));
        } else {
          li.append(button("remove", () => {
            if (confirm(`remove ${m.name} from the room?`)) {
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
      case "ready": return `${who} is ready`;
      case "play": return "play begins";
      case "draw": return `${who} drew a card`;
      case "take": return `${who} took the ${p.value}`;
      case "swap": return `${who} swapped into card ${p.slot + 1}, discarding ${p.discarded}`;
      case "discard": return `${who} discarded ${p.value}`;
      case "peek": return `${who} peeked at ${owner(p.seat)} card ${p.slot + 1}`;
      case "blind_swap": return `${who} blind swapped with ${owner(p.target_seat)} card ${p.target_slot + 1}`;
      case "look_swap": return `${who} swapped with ${owner(p.target_seat)} card ${p.target_slot + 1}`;
      case "skip": return `${who} skipped the move`;
      case "match": return p.ok ? `${who} matched ${owner(p.seat)} ${p.value}` : `${who} missed a match on ${owner(p.seat)} card ${p.slot + 1} and took a penalty`;
      case "komino": return `${who} called KOMINO`;
      case "forfeit": return `${who} left the game`;
      case "away_skip": return `${who} was away; turn skipped`;
      case "scored": return `game over: ${(p.winners || []).map((w) => (w === me ? "you" : nameOf(w))).join(", ") || "no winner"}`;
    }
    return e.kind;
  }

  function renderLog(g) {
    $("log").innerHTML = (view.events || []).map((e) => `<li>${esc(describe(e, g))}</li>`).join("");
  }

  function renderStats() {
    const cols = [["games_played", "games"], ["wins", "wins"], ["komino_calls", "calls"], ["komino_wins", "call wins"],
      ["matches", "matches"], ["failed_matches", "missed"], ["special_moves", "specials"], ["cards_interacted", "cards"], ["forfeits", "forfeits"]];
    const rows = view.stats || [];
    if (!rows.length) return ($("stats").innerHTML = "<tr><td>no games yet</td></tr>");
    const gone = new Set(view.members.filter((m) => m.left || m.removed).map((m) => m.id));
    $("stats").innerHTML = `<tr><th>player</th>${cols.map((c) => `<th>${c[1]}</th>`).join("")}</tr>` +
      rows.map((r) => `<tr><td>${esc(r.name)}${gone.has(r.player) ? " (gone)" : ""}</td>${cols.map((c) => `<td>${r[c[0]]}</td>`).join("")}</tr>`).join("");
  }

  function render() {
    if (!view) return;
    const g = game();
    $("code").textContent = view.room.code;
    const amHost = view.room.host === me;
    const canStart = amHost && (!g || g.status === "scored");
    $("start").hidden = !canStart;
    $("start").textContent = g ? "next game" : "start game";
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
    const g = game();
    // a pending action that no longer applies is dropped, with the reason
    if (sel && g) {
      const now = sel.key.startsWith("m:") ? matchKey(g) : turnKey(g);
      if (now !== sel.key) {
        sel = null;
        matchMode = false;
        $("confirm").hidden = true;
        toast(before && before.discard_seq !== g.discard_seq ? "too late: the discard changed" : "that move is no longer available");
      }
    }
    render();
  }

  // ---------------------------------------------------------------- socket

  function connect() {
    const proto = location.protocol === "https:" ? "wss" : "ws";
    ws = new WebSocket(`${proto}://${location.host}/komino/r/${code}/ws`);
    ws.onopen = () => {
      backoff = 500;
      $("status").classList.remove("offline");
    };
    ws.onmessage = (msg) => {
      const data = JSON.parse(msg.data);
      if (data.type === "view") applyView(data.view);
      else if (data.type === "removed") return gone("removed", "The host removed you from this room.");
      else if (data.type === "result" && !data.ok) toast(data.code === "too_late" ? "too late, someone matched first"
        : data.code === "stale" ? "the turn moved on; check the table and choose again" : data.message);
      else if (data.type === "error") toast(data.message);
    };
    ws.onclose = () => {
      if ($("gone").hidden === false) return;
      $("status").textContent = "reconnecting...";
      setTimeout(connect, backoff);
      backoff = Math.min(backoff * 2, 10000);
    };
  }

  function gone(title, text) {
    $("lobby").hidden = true;
    $("room").hidden = true;
    $("gone").hidden = false;
    $("gone-title").textContent = title;
    $("gone-text").textContent = text;
    if (ws) {
      ws.onclose = null;
      ws.close();
    }
  }

  // ---------------------------------------------------------------- wiring

  $("confirm-cancel").addEventListener("click", clearSel);
  $("confirm-ok").addEventListener("click", () => {
    if (sel && sel.action) send(sel.action);
    clearSel();
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") clearSel();
  });

  $("name").addEventListener("change", async (e) => {
    try {
      const r = await api("POST", "/komino/api/me", { name: e.target.value });
      e.target.value = r.name;
    } catch (err) {
      toast(err.message);
    }
  });

  $("create").addEventListener("click", async () => {
    try {
      const v = await api("POST", "/komino/api/rooms");
      location.href = `/komino/r/${v.room.code}`;
    } catch (err) {
      toast(err.message);
    }
  });
  $("join-form").addEventListener("submit", (e) => {
    e.preventDefault();
    const c = $("join-code").value.trim().toUpperCase();
    if (c) location.href = `/komino/r/${c}`;
  });
  $("copy").addEventListener("click", async () => {
    try {
      await navigator.clipboard.writeText(view.room.url);
      toast("link copied");
    } catch (_) {
      toast(view.room.url);
    }
  });
  $("start").addEventListener("click", () => send({ type: "start" }));
  $("leave").addEventListener("click", async () => {
    if (!confirm("leave this room? if you are in a game you forfeit it.")) return;
    await api("POST", `/komino/api/rooms/${code}/leave`).catch(() => {});
    if (ws) {
      ws.onclose = null;
      ws.close();
    }
    location.href = "/komino";
  });

  // countdowns and peek timeouts
  setInterval(() => {
    if (view && game() && game().status !== "scored") render();
  }, 1000);

  async function main() {
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
    try {
      applyView(await api("POST", `/komino/api/rooms/${code}/join`));
    } catch (err) {
      if (err.code === "removed") return gone("removed", "The host removed you from this room.");
      return gone("room not found", `There is no room with code ${code}.`);
    }
    connect();
  }

  main();
})();
