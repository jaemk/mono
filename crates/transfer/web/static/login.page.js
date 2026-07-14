// Page controller for login.html — the sign-in flow (Google + email/password).
//
// Structured as a factory, createLoginPage(win), that binds to a given window
// (its document, location, Transfer helpers). All state lives in the returned
// closure, so unit tests can build an independent instance against a jsdom
// window without leaking global state between cases.
//
// Browser: a single instance is created from the real `window`, exposed as
// window.TransferLogin, and init() runs the auth/defaults bootstrap and wires
// DOM events on load.
// Node (tests): `createLoginPage` is exported via CommonJS.

(function () {
  'use strict';

  function createLoginPage(win) {
    const doc = win.document;
    const T = win.Transfer;

    let googleClientId = null;

    const $ = (id) => doc.getElementById(id);
    const redirectTarget = () =>
      new URLSearchParams(win.location.search).get('redirect') || '/transfer/my';

    // ── Error ─────────────────────────────────────────────────────────────────
    function showError(msg) {
      const e = $('error-box');
      e.textContent = msg;
      e.style.display = '';
    }
    function hideError() {
      $('error-box').style.display = 'none';
    }

    // ── Google Sign-In ────────────────────────────────────────────────────────
    function loadGSI(clientId) {
      const s = doc.createElement('script');
      s.src = 'https://accounts.google.com/gsi/client';
      s.async = true;
      s.onload = () => {
        google.accounts.id.initialize({ client_id: clientId, callback: onGoogleCredential });
        google.accounts.id.renderButton($('google-btn-container'), {
          type: 'standard', size: 'large', width: 356,
        });
      };
      doc.head.appendChild(s);
    }

    async function onGoogleCredential(resp) {
      hideError();
      try {
        await T.apiJson('/auth/google', { id_token: resp.credential });
        win.location.href = redirectTarget();
      } catch (e) {
        showError(e.message || 'Google sign-in failed');
      }
    }

    // ── Email / password login ────────────────────────────────────────────────
    async function doLogin() {
      const email    = $('login-email').value.trim();
      const password = $('login-password').value;
      if (!email || !password) return showError('Enter your email and password.');
      const btn = $('login-btn');
      btn.disabled = true; btn.textContent = 'Signing in…';
      hideError();
      try {
        await T.apiJson('/auth/login', { email, password });
        win.location.href = redirectTarget();
      } catch (e) {
        showError(e.message || 'Sign in failed');
        btn.disabled = false; btn.textContent = 'Sign in';
      }
    }

    // ── Init: auth check, load defaults, wire DOM events ──────────────────────
    async function init() {
      const me = await T.checkAuth();
      if (me) {
        win.location.href = redirectTarget();
        return;
      }
      T.updateTopbar(null);

      $('login-btn').addEventListener('click', doLogin);
      $('login-password').addEventListener('keydown', (e) => {
        if (e.key === 'Enter') doLogin();
      });

      try {
        const d = await T.apiGet('/upload/defaults');
        if (d.google_enabled && d.google_client_id) {
          googleClientId = d.google_client_id;
          $('google-section').style.display = '';
          loadGSI(d.google_client_id);
        }
        if (d.email_enabled) {
          $('email-section').style.display = 'flex';
        }
        if (d.google_enabled && d.email_enabled) {
          $('or-divider').style.display = '';
        }
        if (!d.google_enabled && !d.email_enabled) {
          showError('Authentication is not configured on this server.');
        }
      } catch (e) {
        showError('Failed to load config: ' + (e.message || e));
      }
    }

    return {
      init,
      doLogin,
      onGoogleCredential,
      loadGSI,
      showError,
      hideError,
      get googleClientId() { return googleClientId; },
    };
  }

  // ── Browser bootstrap ───────────────────────────────────────────────────────
  if (typeof window !== 'undefined') {
    const page = createLoginPage(window);
    window.TransferLogin = page;
    page.init();
  }

  if (typeof module !== 'undefined' && module.exports) {
    module.exports = { createLoginPage };
  }
})();
