/* mapour frontend: a single small vanilla-js app.
   views: landing (auth + your maps), join screen, and the map shell. */

(() => {
  "use strict";

  const API = "/mapour/api";

  // ------------------------------------------------------------------
  // state
  // ------------------------------------------------------------------

  const state = {
    me: null, // { user, anonymous, orgs }
    org: null, // { org, member, categories, link } for the open org
    pins: [],
    filters: new Set(),
    addCat: null, // category id currently being placed
    map: null,
    pinLayer: null,
    admin: { requests: [], pendingPhotos: [], invites: [], members: [] },
  };

  // ------------------------------------------------------------------
  // helpers
  // ------------------------------------------------------------------

  const $ = (sel, root) => (root || document).querySelector(sel);
  const app = () => $("#app");

  function esc(s) {
    return String(s == null ? "" : s)
      .replaceAll("&", "&amp;")
      .replaceAll("<", "&lt;")
      .replaceAll(">", "&gt;")
      .replaceAll('"', "&quot;")
      .replaceAll("'", "&#39;");
  }

  async function api(path, opts = {}) {
    const init = { method: opts.method || "GET", headers: {} };
    if (opts.body !== undefined) {
      if (opts.raw) {
        init.body = opts.body;
        if (opts.contentType) init.headers["content-type"] = opts.contentType;
      } else {
        init.body = JSON.stringify(opts.body);
        init.headers["content-type"] = "application/json";
      }
    }
    const resp = await fetch(API + path, init);
    let data = null;
    try {
      data = await resp.json();
    } catch (_) {
      /* non-json response */
    }
    if (!resp.ok) {
      throw new Error((data && data.error) || `request failed (${resp.status})`);
    }
    return data;
  }

  function toast(msg, isError) {
    const t = document.createElement("div");
    t.className = "toast" + (isError ? " error" : "");
    t.textContent = msg;
    $("#toasts").appendChild(t);
    setTimeout(() => t.remove(), 4200);
  }

  const oops = (e) => toast(e.message || String(e), true);

  function fmtCoords(lat, lng) {
    const ns = lat >= 0 ? "N" : "S";
    const ew = lng >= 0 ? "E" : "W";
    return `${Math.abs(lat).toFixed(4)}° ${ns}, ${Math.abs(lng).toFixed(4)}° ${ew}`;
  }

  function gmapsUrl(lat, lng) {
    return `https://www.google.com/maps?q=${lat.toFixed(6)},${lng.toFixed(6)}`;
  }

  function setUrlOrg(pid) {
    const url = pid ? `/mapour?org=${encodeURIComponent(pid)}` : "/mapour";
    history.replaceState(null, "", url);
  }

  // remembered across the login/register dance
  const pending = {
    get invite() {
      return localStorage.getItem("mapour_pending_invite");
    },
    set invite(v) {
      if (v) localStorage.setItem("mapour_pending_invite", v);
      else localStorage.removeItem("mapour_pending_invite");
    },
    get org() {
      return localStorage.getItem("mapour_pending_org");
    },
    set org(v) {
      if (v) localStorage.setItem("mapour_pending_org", v);
      else localStorage.removeItem("mapour_pending_org");
    },
  };

  // ------------------------------------------------------------------
  // boot
  // ------------------------------------------------------------------

  async function boot() {
    const qs = new URLSearchParams(location.search);
    if (qs.get("verified")) toast("Email verified");

    try {
      state.me = await api("/me");
    } catch (e) {
      state.me = { user: null, anonymous: false, orgs: [] };
    }

    const invite = qs.get("invite") || pending.invite;
    if (invite) {
      if (state.me.user) {
        pending.invite = null;
        try {
          const res = await api("/invites/accept", { method: "POST", body: { token: invite } });
          toast(`Joined ${res.org.name}`);
          state.me = await api("/me");
          await openOrg(res.org.public_id);
          return;
        } catch (e) {
          oops(e);
        }
      } else {
        pending.invite = invite;
        renderLanding("You've been invited to a map. Sign in or create an account to accept.");
        return;
      }
    }

    const pid = qs.get("org") || pending.org;
    if (pid) {
      pending.org = null;
      await openOrg(pid);
      return;
    }
    renderLanding();
  }

  async function afterAuth() {
    // fold any anonymous-cookie memberships into the account
    try {
      await api("/claim", { method: "POST" });
    } catch (_) {
      /* nothing to claim */
    }
    state.me = await api("/me");
    if (pending.invite || pending.org) {
      await boot();
      return;
    }
    renderLanding();
  }

  // ------------------------------------------------------------------
  // landing: brand panel + (auth | your maps)
  // ------------------------------------------------------------------

  function brandPanel() {
    return `
      <div class="brand-panel">
        <div class="wordmark">map<span class="dot">•</span>our</div>
        <div class="tagline">One world map, every pin your team calls <em>home</em>.</div>
        <div class="foot">where we live · where we work · our favorite places</div>
      </div>`;
  }

  function renderLanding(note) {
    teardownMap();
    setUrlOrg(null);
    const signedIn = !!state.me.user;
    const hasOrgs = state.me.orgs.length > 0;
    app().innerHTML = `
      <div class="landing">
        ${brandPanel()}
        <div class="entry-panel">
          <div class="entry-card">
            ${note ? `<p class="muted">${esc(note)}</p>` : ""}
            ${signedIn || hasOrgs ? homeCard() : authCard()}
          </div>
        </div>
      </div>`;
    if (signedIn || hasOrgs) wireHome();
    else wireAuth();
  }

  // --- auth forms ---

  function authCard() {
    return `
      <h2>Welcome</h2>
      <p class="sub">Sign in to map your org, or spin up a throwaway map with no account at all.</p>
      <div class="tabs">
        <button type="button" class="active" data-tab="signin">Sign in</button>
        <button type="button" data-tab="register">Create account</button>
      </div>
      <form id="auth-form">
        <label class="field" id="name-field" hidden>Your name
          <input type="text" name="name" autocomplete="name" maxlength="100" />
        </label>
        <label class="field">Email
          <input type="email" name="email" autocomplete="email" required />
        </label>
        <label class="field">Password <span class="hint" id="pw-hint" hidden>(at least 8 characters)</span>
          <input type="password" name="password" autocomplete="current-password" required />
        </label>
        <div class="error-note" id="auth-error"></div>
        <button class="btn" type="submit" id="auth-submit">Sign in</button>
      </form>
      <div class="divider">or, no account needed</div>
      ${anonMapForm()}`;
  }

  function anonMapForm() {
    return `
      <details class="create-org">
        <summary>Start an anonymous map</summary>
        <form id="anon-org-form">
          <label class="field">Map name
            <input type="text" name="name" required maxlength="100" placeholder="Team offsite 2026" />
          </label>
          <label class="field">Your display name
            <input type="text" name="display_name" maxlength="100" placeholder="Ada" />
          </label>
          <label class="field">Delete after <span class="hint">(days, optional — blank keeps it around)</span>
            <input type="number" name="ttl_days" min="1" max="365" placeholder="30" />
          </label>
          <button class="btn" type="submit">Create map &amp; get link</button>
        </form>
      </details>
      <p class="muted">Anyone with the link can join an anonymous map — no email, no password.</p>`;
  }

  function wireAuth() {
    let mode = "signin";
    document.querySelectorAll(".tabs button").forEach((b) => {
      b.addEventListener("click", () => {
        mode = b.dataset.tab;
        document.querySelectorAll(".tabs button").forEach((x) => x.classList.toggle("active", x === b));
        $("#name-field").hidden = mode !== "register";
        $("#pw-hint").hidden = mode !== "register";
        $("#auth-submit").textContent = mode === "register" ? "Create account" : "Sign in";
        $("#auth-form input[name=name]").required = mode === "register";
      });
    });
    $("#auth-form").addEventListener("submit", async (e) => {
      e.preventDefault();
      const f = new FormData(e.target);
      $("#auth-error").textContent = "";
      try {
        if (mode === "register") {
          const res = await api("/register", {
            method: "POST",
            body: { email: f.get("email"), name: f.get("name"), password: f.get("password") },
          });
          if (res.verification_required) toast("Check your email for a verification link");
        } else {
          await api("/login", {
            method: "POST",
            body: { email: f.get("email"), password: f.get("password") },
          });
        }
        await afterAuth();
      } catch (err) {
        $("#auth-error").textContent = err.message;
      }
    });
    wireAnonOrgForm();
  }

  function wireAnonOrgForm() {
    const form = $("#anon-org-form");
    if (!form) return;
    form.addEventListener("submit", async (e) => {
      e.preventDefault();
      const f = new FormData(form);
      try {
        const res = await api("/orgs", {
          method: "POST",
          body: {
            name: f.get("name"),
            anonymous: true,
            display_name: f.get("display_name") || null,
            ttl_days: f.get("ttl_days") ? Number(f.get("ttl_days")) : null,
          },
        });
        state.me = await api("/me");
        await openOrg(res.org.public_id);
        toast("Map created — use Share to invite people");
      } catch (err) {
        oops(err);
      }
    });
  }

  // --- signed-in home ---

  function homeCard() {
    const who = state.me.user
      ? `${esc(state.me.user.name)} · ${esc(state.me.user.email)}`
      : "browsing anonymously";
    const orgs = state.me.orgs
      .map(
        (o) => `
        <li>
          <button type="button" class="org-open" data-pid="${esc(o.public_id)}">
            <span>${esc(o.name)}
              <span class="org-meta">${esc(o.public_id)}${o.kind === "anon" ? " · anonymous" : ""}</span>
            </span>
            <span class="badge ${o.role === "admin" ? "admin" : ""}">${esc(o.role)}</span>
          </button>
        </li>`
      )
      .join("");
    return `
      <h2>Your maps</h2>
      <p class="sub">${who}</p>
      ${
        !state.me.user && state.me.orgs.length
          ? `<p class="muted">You're anonymous — <a href="#" id="anon-upgrade">create an account</a> to keep access to these maps from any device.</p>`
          : ""
      }
      <ul class="org-list">${orgs || `<li class="muted">No maps yet — create one below.</li>`}</ul>
      <details class="create-org" ${state.me.orgs.length ? "" : "open"}>
        <summary>Create an org</summary>
        <form id="create-org-form">
          <label class="field">Org name
            <input type="text" name="name" required maxlength="100" placeholder="Woolworth Inc" />
          </label>
          <label class="field">Open to email domain <span class="hint">(optional — anyone @this domain can join)</span>
            <input type="text" name="allowed_email_domain" placeholder="woolworth.com" />
          </label>
          <button class="btn" type="submit" ${state.me.user ? "" : "disabled"}>Create org</button>
          ${state.me.user ? "" : `<p class="muted">Creating a standard org requires an account.</p>`}
        </form>
      </details>
      ${anonMapForm()}
      <div class="divider">join an existing org</div>
      <form id="join-form" class="inline-form">
        <input type="text" name="pid" placeholder="org id, e.g. 3fa9c04b11de" required />
        <button class="btn secondary" type="submit">Open</button>
      </form>
      <div class="divider"></div>
      ${
        state.me.user
          ? `<button class="btn secondary" id="logout">Sign out</button>`
          : `<button class="btn secondary" id="anon-signin">Sign in / create account</button>`
      }`;
  }

  function wireHome() {
    document.querySelectorAll(".org-open").forEach((b) =>
      b.addEventListener("click", () => openOrg(b.dataset.pid).catch(oops))
    );
    const create = $("#create-org-form");
    if (create) {
      create.addEventListener("submit", async (e) => {
        e.preventDefault();
        const f = new FormData(create);
        try {
          const res = await api("/orgs", {
            method: "POST",
            body: {
              name: f.get("name"),
              allowed_email_domain: f.get("allowed_email_domain") || null,
            },
          });
          state.me = await api("/me");
          await openOrg(res.org.public_id);
        } catch (err) {
          oops(err);
        }
      });
    }
    wireAnonOrgForm();
    $("#join-form").addEventListener("submit", (e) => {
      e.preventDefault();
      const pid = new FormData(e.target).get("pid").trim();
      if (pid) openOrg(pid).catch(oops);
    });
    const logout = $("#logout");
    if (logout) {
      logout.addEventListener("click", async () => {
        await api("/logout", { method: "POST" }).catch(() => {});
        location.href = "/mapour";
      });
    }
    const signin = $("#anon-signin") || $("#anon-upgrade");
    if (signin) {
      signin.addEventListener("click", (e) => {
        e.preventDefault();
        renderAuthOnly();
      });
    }
  }

  function renderAuthOnly() {
    app().innerHTML = `
      <div class="landing">
        ${brandPanel()}
        <div class="entry-panel"><div class="entry-card">${authCard()}</div></div>
      </div>`;
    wireAuth();
  }

  // ------------------------------------------------------------------
  // join screen (know the link/id but not a member yet)
  // ------------------------------------------------------------------

  function renderJoin(info, pid) {
    teardownMap();
    setUrlOrg(pid);
    const anonOrg = info.org.kind === "anon";
    let body;
    if (anonOrg) {
      body = `
        <p class="muted">This is an anonymous map — pick a display name and jump in.</p>
        <form id="join-org-form">
          <label class="field">Display name
            <input type="text" name="display_name" maxlength="100"
              value="${esc(state.me.user ? state.me.user.name : "")}" placeholder="Ada" />
          </label>
          <button class="btn" type="submit">Join map</button>
        </form>`;
    } else if (!state.me.user) {
      body = `
        <p class="muted">You need an account to join this org.</p>
        <button class="btn" id="join-signin">Sign in / create account</button>`;
    } else if (info.pending_request) {
      body = `<p class="muted">Your request to join is waiting on an org admin. Check back later.</p>`;
    } else if (info.can_join) {
      body = `
        <p class="muted">Your email domain is on this org's allowlist — you can join right away.</p>
        <form id="join-org-form"><button class="btn" type="submit">Join org</button></form>`;
    } else {
      body = `
        <p class="muted">This org is invite-only. You can ask an admin to let you in.</p>
        <form id="join-org-form"><button class="btn" type="submit">Request access</button></form>`;
    }

    app().innerHTML = `
      <div class="landing">
        ${brandPanel()}
        <div class="entry-panel">
          <div class="entry-card">
            <h2>${esc(info.org.name)}</h2>
            <p class="sub mono">${esc(info.org.public_id)}</p>
            ${body}
            <div class="divider"></div>
            <button class="btn secondary" id="join-back">Back</button>
          </div>
        </div>
      </div>`;

    $("#join-back").addEventListener("click", () => renderLanding());
    const signin = $("#join-signin");
    if (signin) {
      signin.addEventListener("click", () => {
        pending.org = pid;
        renderAuthOnly();
      });
    }
    const form = $("#join-org-form");
    if (form) {
      form.addEventListener("submit", async (e) => {
        e.preventDefault();
        const f = new FormData(form);
        try {
          const res = await api(`/orgs/${encodeURIComponent(pid)}/join`, {
            method: "POST",
            body: { display_name: f.get("display_name") || null },
          });
          if (res.joined) {
            state.me = await api("/me").catch(() => state.me);
            await openOrg(pid);
          } else {
            toast("Request sent — an org admin has to approve it");
            renderLanding();
          }
        } catch (err) {
          oops(err);
        }
      });
    }
  }

  // ------------------------------------------------------------------
  // map shell
  // ------------------------------------------------------------------

  async function openOrg(pid) {
    let info;
    try {
      info = await api(`/orgs/${encodeURIComponent(pid)}`);
    } catch (e) {
      oops(e);
      renderLanding();
      return;
    }
    if (!info.member) {
      renderJoin(info, pid);
      return;
    }
    state.org = info;
    state.addCat = null;
    const saved = localStorage.getItem(`mapour_filters_${pid}`);
    const keys = info.categories.map((c) => c.key);
    state.filters = new Set(
      saved ? JSON.parse(saved).filter((k) => keys.includes(k)) : ["live", "work"].filter((k) => keys.includes(k))
    );
    renderMapShell();
    await reloadPins();
    if (info.member.role === "admin") await reloadAdmin();
  }

  function teardownMap() {
    if (state.map) {
      state.map.remove();
      state.map = null;
      state.pinLayer = null;
    }
    state.org = null;
  }

  function renderMapShell() {
    if (state.map) {
      state.map.remove();
      state.map = null;
    }
    const o = state.org;
    app().innerHTML = `
      <div class="map-shell">
        <div class="topbar">
          <div class="wordmark" id="go-home" title="Back to your maps">map<span class="dot">•</span>our</div>
          <div class="org-name">${esc(o.org.name)}</div>
          <div class="org-id">${esc(o.org.public_id)}</div>
          <div class="spacer"></div>
          <button class="btn small" id="topbar-account">
            ${state.me.user ? esc(state.me.user.name) : "anonymous"}
          </button>
        </div>
        <div class="map-wrap">
          <div id="map"></div>
          <aside class="side-panel" id="side-panel"></aside>
          <button class="btn panel-toggle" id="panel-toggle">Legend</button>
        </div>
      </div>`;

    $("#go-home").addEventListener("click", () => renderLanding());
    $("#topbar-account").addEventListener("click", () => renderLanding());
    $("#panel-toggle").addEventListener("click", () =>
      $("#side-panel").classList.toggle("collapsed")
    );
    if (window.matchMedia("(max-width: 760px)").matches) {
      $("#side-panel").classList.add("collapsed");
    }

    state.map = L.map("map", { worldCopyJump: true, minZoom: 2, zoomControl: true }).setView(
      [24, 12],
      2
    );
    L.tileLayer("https://{s}.basemaps.cartocdn.com/rastertiles/voyager_nolabels/{z}/{x}/{y}{r}.png", {
      attribution:
        '&copy; <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a> &copy; <a href="https://carto.com/attributions">CARTO</a>',
      maxZoom: 17,
    }).addTo(state.map);
    L.tileLayer("https://{s}.basemaps.cartocdn.com/rastertiles/voyager_only_labels/{z}/{x}/{y}{r}.png", {
      maxZoom: 17,
      pane: "shadowPane",
    }).addTo(state.map);
    state.pinLayer = L.layerGroup().addTo(state.map);

    state.map.on("click", (e) => {
      if (state.addCat != null) openNewPinPopup(e.latlng);
    });

    renderSidePanel();
  }

  // ------------------------------------------------------------------
  // side panel (legend = filters, add pin, share, members, admin)
  // ------------------------------------------------------------------

  function isAdmin() {
    return state.org && state.org.member.role === "admin";
  }

  function pinCounts() {
    const counts = {};
    for (const p of state.pins) counts[p.category_key] = (counts[p.category_key] || 0) + 1;
    return counts;
  }

  function myCategoryCounts() {
    const counts = {};
    for (const p of state.pins) {
      if (p.is_yours) counts[p.category_id] = (counts[p.category_id] || 0) + 1;
    }
    return counts;
  }

  function renderSidePanel() {
    const o = state.org;
    const counts = pinCounts();
    const mine = myCategoryCounts();
    const admin = isAdmin();

    let lastGrp = null;
    const legendRows = o.categories
      .map((c) => {
        const grpHeader =
          c.grp && c.grp !== lastGrp ? `<div class="legend-row grp">${esc(c.grp)}</div>` : "";
        lastGrp = c.grp;
        return `
          ${grpHeader}
          <label class="legend-row">
            <input type="checkbox" data-filter="${esc(c.key)}" ${state.filters.has(c.key) ? "checked" : ""} />
            ${
              admin
                ? `<input type="color" value="${esc(c.color)}" data-cat-color="${c.id}" title="Category color" />`
                : `<span class="swatch" style="background:${esc(c.color)}"></span>`
            }
            <span>${esc(c.name)}</span>
            <span class="count">${counts[c.key] || 0}</span>
          </label>`;
      })
      .join("");

    const addOptions = o.categories
      .map((c) => {
        const maxed = c.max_per_member != null && (mine[c.id] || 0) >= c.max_per_member;
        return `<option value="${c.id}" ${maxed ? "disabled" : ""}>${esc(c.name)}${
          maxed ? " (placed)" : ""
        }</option>`;
      })
      .join("");

    const expires = o.org.expires_at
      ? `<div class="muted mono">self-destructs ${new Date(o.org.expires_at).toISOString().slice(0, 10)}</div>`
      : "";

    $("#side-panel").innerHTML = `
      <section>
        <h3>Legend &amp; filters</h3>
        ${legendRows}
        <div class="muted" style="margin-top:8px">
          <a href="#" id="filter-live-work">live &amp; work</a> · <a href="#" id="filter-all">everything</a>
        </div>
      </section>
      <section>
        <h3>Add a pin</h3>
        <div class="inline-form add-pin-row">
          <select id="add-cat">${addOptions}</select>
          <button class="btn small" id="add-start">Place</button>
        </div>
        <div class="add-pin-hint" id="add-hint" hidden>
          Click the map to drop your pin — <a href="#" id="add-cancel">cancel</a>
        </div>
      </section>
      <section>
        <h3>Share</h3>
        <div class="share-line">
          <input type="text" readonly value="${esc(o.link)}" id="share-url" />
          <button class="btn small secondary" id="share-copy">Copy</button>
        </div>
        ${
          o.org.kind === "anon"
            ? `<div class="muted" style="margin-top:6px">Anyone with this link can join.</div>`
            : `<div class="muted" style="margin-top:6px">Members can open the map here; others can request access.</div>`
        }
        ${expires}
      </section>
      <section>
        <h3>Members</h3>
        <div id="members-box" class="muted">loading…</div>
      </section>
      ${
        admin
          ? `
      <section>
        <h3>Photo approvals</h3>
        <div id="approvals-box" class="muted">loading…</div>
      </section>
      <section>
        <h3>Join requests</h3>
        <div id="requests-box" class="muted">loading…</div>
      </section>
      ${
        state.org.org.kind === "standard"
          ? `<section>
        <h3>Invites</h3>
        <form id="invite-form" class="inline-form">
          <input type="email" name="email" placeholder="teammate@company.com" required />
          <button class="btn small" type="submit">Invite</button>
        </form>
        <div id="invites-box"></div>
      </section>`
          : ""
      }`
          : ""
      }
      <section>
        <button class="btn small danger" id="leave-org">Leave this map</button>
      </section>`;

    // filters
    document.querySelectorAll("[data-filter]").forEach((cb) =>
      cb.addEventListener("change", () => {
        if (cb.checked) state.filters.add(cb.dataset.filter);
        else state.filters.delete(cb.dataset.filter);
        saveFilters();
        drawPins();
      })
    );
    $("#filter-live-work").addEventListener("click", (e) => {
      e.preventDefault();
      state.filters = new Set(["live", "work"]);
      saveFilters();
      renderSidePanel();
      drawPins();
    });
    $("#filter-all").addEventListener("click", (e) => {
      e.preventDefault();
      state.filters = new Set(state.org.categories.map((c) => c.key));
      saveFilters();
      renderSidePanel();
      drawPins();
    });

    // category colors (admin)
    document.querySelectorAll("[data-cat-color]").forEach((inp) =>
      inp.addEventListener("change", async () => {
        try {
          await api(`/orgs/${state.org.org.public_id}/categories/${inp.dataset.catColor}`, {
            method: "POST",
            body: { color: inp.value },
          });
          const cat = state.org.categories.find((c) => c.id === Number(inp.dataset.catColor));
          if (cat) cat.color = inp.value;
          state.pins.forEach((p) => {
            if (p.category_id === Number(inp.dataset.catColor)) p.color = inp.value;
          });
          drawPins();
        } catch (err) {
          oops(err);
        }
      })
    );

    // add pin
    $("#add-start").addEventListener("click", () => {
      const sel = $("#add-cat");
      if (!sel.value) return;
      state.addCat = Number(sel.value);
      $("#add-hint").hidden = false;
      $("#map").style.cursor = "crosshair";
    });
    $("#add-cancel").addEventListener("click", (e) => {
      e.preventDefault();
      cancelAddMode();
    });

    // share
    $("#share-copy").addEventListener("click", async () => {
      try {
        await navigator.clipboard.writeText($("#share-url").value);
        toast("Link copied");
      } catch (_) {
        $("#share-url").select();
        document.execCommand("copy");
        toast("Link copied");
      }
    });

    // leave
    $("#leave-org").addEventListener("click", async () => {
      if (!confirm("Leave this map? Your pins will be removed.")) return;
      try {
        await api(`/orgs/${state.org.org.public_id}/members/${state.org.member.id}`, {
          method: "DELETE",
        });
        state.me = await api("/me").catch(() => state.me);
        renderLanding();
      } catch (err) {
        oops(err);
      }
    });

    // invites
    const inviteForm = $("#invite-form");
    if (inviteForm) {
      inviteForm.addEventListener("submit", async (e) => {
        e.preventDefault();
        const email = new FormData(inviteForm).get("email");
        try {
          const res = await api(`/orgs/${state.org.org.public_id}/invites`, {
            method: "POST",
            body: { email },
          });
          await navigator.clipboard.writeText(res.link).catch(() => {});
          toast(`Invite link for ${res.email} copied — send it along`);
          prompt("Share this invite link (email sending isn't wired up yet):", res.link);
          inviteForm.reset();
          await reloadAdmin();
        } catch (err) {
          oops(err);
        }
      });
    }

    renderMembersBox();
    if (admin) renderAdminBoxes();
  }

  function saveFilters() {
    localStorage.setItem(
      `mapour_filters_${state.org.org.public_id}`,
      JSON.stringify([...state.filters])
    );
  }

  function cancelAddMode() {
    state.addCat = null;
    const hint = $("#add-hint");
    if (hint) hint.hidden = true;
    const map = $("#map");
    if (map) map.style.cursor = "";
  }

  // ------------------------------------------------------------------
  // members + admin boxes
  // ------------------------------------------------------------------

  async function reloadMembers() {
    const res = await api(`/orgs/${state.org.org.public_id}/members`);
    state.admin.members = res.members;
  }

  async function renderMembersBox() {
    try {
      await reloadMembers();
    } catch (e) {
      $("#members-box").textContent = "could not load members";
      return;
    }
    const admin = isAdmin();
    const rows = state.admin.members
      .map((m) => {
        const roleCtl =
          admin && !m.is_you && !m.anonymous
            ? `<select data-role-for="${m.id}">
                 <option value="member" ${m.role === "member" ? "selected" : ""}>member</option>
                 <option value="admin" ${m.role === "admin" ? "selected" : ""}>admin</option>
               </select>`
            : `<span class="badge ${m.role === "admin" ? "admin" : ""}">${esc(m.role)}</span>`;
        const removeCtl =
          admin && !m.is_you
            ? `<button class="btn small danger" data-remove-member="${m.id}">remove</button>`
            : "";
        return `
          <div class="admin-item">
            <div class="who">
              <div class="name">${esc(m.display_name)}${m.is_you ? " (you)" : ""}</div>
              <div class="sub">${esc(m.email || (m.anonymous ? "anonymous" : ""))}</div>
            </div>
            <div class="actions">${roleCtl}${removeCtl}</div>
          </div>`;
      })
      .join("");
    $("#members-box").innerHTML = rows || "no members";

    document.querySelectorAll("[data-role-for]").forEach((sel) =>
      sel.addEventListener("change", async () => {
        try {
          await api(`/orgs/${state.org.org.public_id}/members/${sel.dataset.roleFor}/role`, {
            method: "POST",
            body: { role: sel.value },
          });
          toast("Role updated");
          renderMembersBox();
        } catch (err) {
          oops(err);
          renderMembersBox();
        }
      })
    );
    document.querySelectorAll("[data-remove-member]").forEach((b) =>
      b.addEventListener("click", async () => {
        if (!confirm("Remove this member and all of their pins?")) return;
        try {
          await api(`/orgs/${state.org.org.public_id}/members/${b.dataset.removeMember}`, {
            method: "DELETE",
          });
          await reloadPins();
          renderMembersBox();
        } catch (err) {
          oops(err);
        }
      })
    );
  }

  async function reloadAdmin() {
    const pid = state.org.org.public_id;
    const [requests, photos, invites] = await Promise.all([
      api(`/orgs/${pid}/requests`).catch(() => ({ requests: [] })),
      api(`/orgs/${pid}/photos/pending`).catch(() => ({ photos: [] })),
      state.org.org.kind === "standard"
        ? api(`/orgs/${pid}/invites`).catch(() => ({ invites: [] }))
        : Promise.resolve({ invites: [] }),
    ]);
    state.admin.requests = requests.requests;
    state.admin.pendingPhotos = photos.photos;
    state.admin.invites = invites.invites;
    renderAdminBoxes();
  }

  function renderAdminBoxes() {
    const pid = state.org.org.public_id;

    const approvals = $("#approvals-box");
    if (approvals) {
      approvals.innerHTML =
        state.admin.pendingPhotos
          .map(
            (p) => `
          <div class="pending-photo">
            <div class="muted">${esc(p.member_name)} · ${esc(p.category_name)}</div>
            <img src="${API}/orgs/${pid}/photos/${p.id}" alt="pending photo" loading="lazy" />
            <div class="actions" style="display:flex;gap:6px">
              <button class="btn small" data-approve-photo="${p.id}">Approve</button>
              <button class="btn small danger" data-reject-photo="${p.id}">Delete</button>
            </div>
          </div>`
          )
          .join("") || `<span class="muted">nothing waiting</span>`;
      approvals.querySelectorAll("[data-approve-photo]").forEach((b) =>
        b.addEventListener("click", async () => {
          try {
            await api(`/orgs/${pid}/photos/${b.dataset.approvePhoto}/approve`, { method: "POST" });
            toast("Photo approved");
            await Promise.all([reloadPins(), reloadAdmin()]);
          } catch (err) {
            oops(err);
          }
        })
      );
      approvals.querySelectorAll("[data-reject-photo]").forEach((b) =>
        b.addEventListener("click", async () => {
          if (!confirm("Delete this photo?")) return;
          try {
            await api(`/orgs/${pid}/photos/${b.dataset.rejectPhoto}`, { method: "DELETE" });
            await Promise.all([reloadPins(), reloadAdmin()]);
          } catch (err) {
            oops(err);
          }
        })
      );
    }

    const requests = $("#requests-box");
    if (requests) {
      requests.innerHTML =
        state.admin.requests
          .map(
            (r) => `
          <div class="admin-item">
            <div class="who">
              <div class="name">${esc(r.name)}</div>
              <div class="sub">${esc(r.email)}</div>
            </div>
            <div class="actions">
              <button class="btn small" data-req-approve="${r.id}">Approve</button>
              <button class="btn small danger" data-req-deny="${r.id}">Deny</button>
            </div>
          </div>`
          )
          .join("") || `<span class="muted">none pending</span>`;
      requests.querySelectorAll("[data-req-approve],[data-req-deny]").forEach((b) =>
        b.addEventListener("click", async () => {
          const id = b.dataset.reqApprove || b.dataset.reqDeny;
          const decision = b.dataset.reqApprove ? "approve" : "deny";
          try {
            await api(`/orgs/${pid}/requests/${id}/${decision}`, { method: "POST" });
            await reloadAdmin();
            renderMembersBox();
          } catch (err) {
            oops(err);
          }
        })
      );
    }

    const invites = $("#invites-box");
    if (invites) {
      invites.innerHTML = state.admin.invites
        .map(
          (i) => `
        <div class="admin-item">
          <div class="who">
            <div class="name">${esc(i.email)}</div>
            <div class="sub">expires ${new Date(i.expires).toISOString().slice(0, 10)}</div>
          </div>
          <div class="actions">
            <button class="btn small danger" data-del-invite="${i.id}">revoke</button>
          </div>
        </div>`
        )
        .join("");
      invites.querySelectorAll("[data-del-invite]").forEach((b) =>
        b.addEventListener("click", async () => {
          try {
            await api(`/orgs/${pid}/invites/${b.dataset.delInvite}`, { method: "DELETE" });
            await reloadAdmin();
          } catch (err) {
            oops(err);
          }
        })
      );
    }
  }

  // ------------------------------------------------------------------
  // pins
  // ------------------------------------------------------------------

  async function reloadPins() {
    const res = await api(`/orgs/${state.org.org.public_id}/pins`);
    state.pins = res.pins;
    drawPins();
    // counts + "placed" markers depend on pins
    renderSidePanel();
  }

  function visiblePins() {
    return state.pins.filter((p) => state.filters.has(p.category_key));
  }

  /* pins sharing a location render as one marker that grows with the count */
  function drawPins() {
    if (!state.pinLayer) return;
    state.pinLayer.clearLayers();
    const groups = new Map();
    for (const p of visiblePins()) {
      const key = `${p.lat.toFixed(4)},${p.lng.toFixed(4)}`;
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key).push(p);
    }
    for (const group of groups.values()) {
      const count = group.length;
      const size = count > 1 ? Math.min(26 + count * 4, 60) : 22;
      const color = group[0].color;
      const icon = L.divIcon({
        className: "",
        html: `<div class="pin-dot" style="background:${esc(color)}">${count > 1 ? count : ""}</div>`,
        iconSize: [size, size],
        iconAnchor: [size / 2, size / 2],
      });
      const marker = L.marker([group[0].lat, group[0].lng], { icon });
      marker.bindPopup(() => popupContent(group), { maxWidth: 290 });
      marker.addTo(state.pinLayer);
    }
  }

  function popupContent(group) {
    const root = document.createElement("div");
    for (const pin of group) root.appendChild(pinBlock(pin));
    return root;
  }

  function pinBlock(pin) {
    const pid = state.org.org.public_id;
    const cat = state.org.categories.find((c) => c.id === pin.category_id);
    const div = document.createElement("div");
    div.className = "popup-pin";

    const photoHtml = pin.photo
      ? `<img class="photo" src="${API}/orgs/${pid}/photos/${pin.photo.id}" alt="pin photo" />
         ${pin.photo.approved ? "" : `<span class="badge pending">awaiting admin approval</span>`}`
      : "";

    div.innerHTML = `
      <div class="head">
        <span class="swatch" style="background:${esc(pin.color)}"></span>
        ${esc(cat ? cat.name : pin.category_key)}
        <span class="by">· ${esc(pin.member_name)}</span>
      </div>
      ${pin.description ? `<div class="desc">${esc(pin.description)}</div>` : ""}
      ${photoHtml}
      <div class="coords">${fmtCoords(pin.lat, pin.lng)}</div>
      <div class="links">
        <a href="${gmapsUrl(pin.lat, pin.lng)}" target="_blank" rel="noopener">Google Maps ↗</a>
      </div>`;

    const links = div.querySelector(".links");
    const canEdit = pin.is_yours || isAdmin();
    if (canEdit) {
      const edit = document.createElement("button");
      edit.textContent = "edit";
      edit.addEventListener("click", () => editPinForm(div, pin));
      links.appendChild(edit);

      const photoBtn = document.createElement("button");
      photoBtn.textContent = pin.photo ? "replace photo" : "add photo";
      photoBtn.addEventListener("click", () => pickAndUploadPhoto(pin));
      links.appendChild(photoBtn);

      if (pin.photo) {
        const delPhoto = document.createElement("button");
        delPhoto.className = "danger";
        delPhoto.textContent = "remove photo";
        delPhoto.addEventListener("click", async () => {
          try {
            await api(`/orgs/${pid}/photos/${pin.photo.id}`, { method: "DELETE" });
            state.map.closePopup();
            await reloadPins();
          } catch (err) {
            oops(err);
          }
        });
        links.appendChild(delPhoto);
      }

      const del = document.createElement("button");
      del.className = "danger";
      del.textContent = "delete pin";
      del.addEventListener("click", async () => {
        if (!confirm("Delete this pin?")) return;
        try {
          await api(`/orgs/${pid}/pins/${pin.id}`, { method: "DELETE" });
          state.map.closePopup();
          await reloadPins();
          if (isAdmin()) await reloadAdmin();
        } catch (err) {
          oops(err);
        }
      });
      links.appendChild(del);
    }
    if (isAdmin() && pin.photo && !pin.photo.approved) {
      const approve = document.createElement("button");
      approve.textContent = "approve photo";
      approve.addEventListener("click", async () => {
        try {
          await api(`/orgs/${pid}/photos/${pin.photo.id}/approve`, { method: "POST" });
          state.map.closePopup();
          await Promise.all([reloadPins(), reloadAdmin()]);
        } catch (err) {
          oops(err);
        }
      });
      links.appendChild(approve);
    }
    return div;
  }

  function editPinForm(container, pin) {
    const pid = state.org.org.public_id;
    container.innerHTML = `
      <form class="popup-form">
        <textarea name="description" maxlength="2000"
          placeholder="What's here?">${esc(pin.description || "")}</textarea>
        <div class="row">
          <button class="btn small secondary" type="button" data-cancel>Cancel</button>
          <button class="btn small" type="submit">Save</button>
        </div>
      </form>`;
    container.querySelector("[data-cancel]").addEventListener("click", () => {
      container.replaceWith(pinBlock(pin));
    });
    container.querySelector("form").addEventListener("submit", async (e) => {
      e.preventDefault();
      const description = new FormData(e.target).get("description");
      try {
        await api(`/orgs/${pid}/pins/${pin.id}`, { method: "POST", body: { description } });
        state.map.closePopup();
        await reloadPins();
      } catch (err) {
        oops(err);
      }
    });
  }

  function pickAndUploadPhoto(pin) {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = "image/jpeg,image/png,image/webp,image/gif";
    input.addEventListener("change", async () => {
      const file = input.files[0];
      if (!file) return;
      try {
        const res = await api(`/orgs/${state.org.org.public_id}/pins/${pin.id}/photo`, {
          method: "POST",
          raw: true,
          body: file,
          contentType: file.type,
        });
        toast(
          res.photo.approved
            ? "Photo added"
            : "Photo uploaded — only you can see it until an admin approves"
        );
        state.map.closePopup();
        await reloadPins();
        if (isAdmin()) await reloadAdmin();
      } catch (err) {
        oops(err);
      }
    });
    input.click();
  }

  function openNewPinPopup(latlng) {
    const pid = state.org.org.public_id;
    const catId = state.addCat;
    const cat = state.org.categories.find((c) => c.id === catId);
    if (!cat) return;

    const node = document.createElement("div");
    node.className = "popup-pin";
    node.innerHTML = `
      <div class="head">
        <span class="swatch" style="background:${esc(cat.color)}"></span>
        New “${esc(cat.name)}” pin
      </div>
      <div class="coords">${fmtCoords(latlng.lat, latlng.lng)}</div>
      <form class="popup-form">
        <textarea name="description" maxlength="2000" placeholder="What's here? (optional)"></textarea>
        <label class="muted">Photo (optional, admin-approved before others see it)
          <input type="file" name="photo" accept="image/jpeg,image/png,image/webp,image/gif" />
        </label>
        <div class="row">
          <button class="btn small secondary" type="button" data-cancel>Cancel</button>
          <button class="btn small" type="submit">Drop pin</button>
        </div>
      </form>`;

    const popup = L.popup({ maxWidth: 290 }).setLatLng(latlng).setContent(node).openOn(state.map);
    node.querySelector("[data-cancel]").addEventListener("click", () => {
      state.map.closePopup(popup);
      cancelAddMode();
    });
    node.querySelector("form").addEventListener("submit", async (e) => {
      e.preventDefault();
      const f = new FormData(e.target);
      // normalize the clicked longitude into [-180, 180] (worldCopyJump
      // can hand back wrapped copies)
      const lng = ((latlng.lng + 540) % 360) - 180;
      try {
        const res = await api(`/orgs/${pid}/pins`, {
          method: "POST",
          body: {
            category_id: catId,
            lat: latlng.lat,
            lng,
            description: f.get("description") || null,
          },
        });
        const file = f.get("photo");
        if (file && file.size > 0) {
          await api(`/orgs/${pid}/pins/${res.pin.id}/photo`, {
            method: "POST",
            raw: true,
            body: file,
            contentType: file.type,
          });
          if (!isAdmin()) toast("Pin dropped — photo awaits admin approval");
          else toast("Pin dropped");
        } else {
          toast("Pin dropped");
        }
        state.map.closePopup(popup);
        cancelAddMode();
        // make sure the new pin's category is visible
        state.filters.add(cat.key);
        saveFilters();
        await reloadPins();
        if (isAdmin()) await reloadAdmin();
      } catch (err) {
        oops(err);
      }
    });
  }

  // ------------------------------------------------------------------

  boot().catch((e) => {
    console.error(e);
    oops(e);
  });
})();
