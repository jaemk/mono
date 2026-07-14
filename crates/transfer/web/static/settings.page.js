// Page controller for settings.html — account info + add-password flow.
//
// Structured as a factory, createSettingsPage(win), that binds to a given
// window (its document, location, Transfer helpers). All state lives in the
// returned closure, so unit tests can build an independent instance against a
// jsdom window without leaking global state between cases.
//
// Browser: a single instance is created from the real `window`, exposed as
// window.TransferSettings, and init() wires DOM events + runs the auth check.
// Node (tests): `createSettingsPage` is exported via CommonJS.

(function () {
  'use strict';

  function createSettingsPage(win) {
    const doc = win.document;
    const T = win.Transfer;

    // ── State ───────────────────────────────────────────────────────────────
    let currentUser = null;

    // ── Init: auth check, populate account fields, wire DOM events ────────────
    async function init() {
      const me = await T.checkAuth();
      if (!me) { win.location.href = '/transfer/login?redirect=/transfer/settings'; return; }
      currentUser = me;
      T.updateTopbar(me);

      doc.getElementById('account-email').textContent = me.email;
      doc.getElementById('account-pw-status').textContent = me.has_password ? 'set' : 'not set';
      doc.getElementById('account-google-status').textContent = me.google_linked ? 'linked' : 'not linked';

      if (!me.has_password) {
        doc.getElementById('add-pw-section').style.display = '';
      }

      doc.getElementById('add-pw-btn').addEventListener('click', doAddPassword);
      doc.getElementById('new-password').addEventListener('keydown', (e) => {
        if (e.key === 'Enter') doAddPassword();
      });
    }

    // ── Add-password flow ─────────────────────────────────────────────────────
    async function doAddPassword() {
      const password = doc.getElementById('new-password').value;
      if (password.length < 8) return showError('Password must be at least 8 characters.');
      const btn = doc.getElementById('add-pw-btn');
      btn.disabled = true; btn.textContent = 'Saving…';
      hideError(); hideSuccess();
      try {
        await T.apiJson('/settings/add-password', { password });
        doc.getElementById('account-pw-status').textContent = 'set';
        doc.getElementById('add-pw-section').style.display = 'none';
        showSuccess('Password added. You can now sign in with email and password.');
      } catch (e) {
        showError(e.message || 'Failed to add password');
        btn.disabled = false; btn.textContent = 'Save password';
      }
    }

    // ── Error / success banners ───────────────────────────────────────────────
    function showError(msg)   { const e = doc.getElementById('error-box');   e.textContent = msg; e.style.display = ''; }
    function showSuccess(msg) { const e = doc.getElementById('success-box'); e.textContent = msg; e.style.display = ''; }
    function hideError()   { doc.getElementById('error-box').style.display = 'none'; }
    function hideSuccess() { doc.getElementById('success-box').style.display = 'none'; }

    return {
      init,
      doAddPassword,
      showError, showSuccess, hideError, hideSuccess,
      // read-only accessor for assertions
      get currentUser() { return currentUser; },
    };
  }

  // ── Browser bootstrap ───────────────────────────────────────────────────────
  if (typeof window !== 'undefined') {
    const page = createSettingsPage(window);
    window.TransferSettings = page;
    page.init();
  }

  if (typeof module !== 'undefined' && module.exports) {
    module.exports = { createSettingsPage };
  }
})();
