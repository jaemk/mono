// Page controller for delete.html — the revoke-and-delete flow.
//
// Structured as a factory, createDeletePage(win), that binds to a given
// window (its document, location, Transfer helpers). All state lives in the
// returned closure, so unit tests can build an independent instance against a
// jsdom window without leaking global state between cases.
//
// Browser: a single instance is created from the real `window`, exposed as
// window.TransferDelete, and init() wires DOM events on load.
// Node (tests): `createDeletePage` is exported via CommonJS.

(function () {
  'use strict';

  // innerHTML restored on the delete button when an error resets it.
  const TRASH_ICON = `<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="3 6 5 6 21 6"/><path d="M19 6l-1 14a2 2 0 01-2 2H8a2 2 0 01-2-2L5 6"/><path d="M10 11v6M14 11v6"/><path d="M9 6V4a1 1 0 011-1h4a1 1 0 011 1v2"/></svg> Delete transfer`;

  function createDeletePage(win) {
    const doc = win.document;
    const T = win.Transfer;
    const params = new URLSearchParams(win.location.search);
    const uploadKey = params.get('key');

    const $ = (id) => doc.getElementById(id);

    // ── Ready check ───────────────────────────────────────────────────────────
    function checkDeleteReady() {
      $('delete-btn').disabled = !$('deletion-pw').value.trim();
    }

    // ── Delete flow ───────────────────────────────────────────────────────────
    async function doDelete() {
      const pw = $('deletion-pw').value;
      if (!pw.trim()) return;

      const btn = $('delete-btn');
      btn.disabled = true;
      btn.innerHTML = 'Deleting…';
      $('error-box').style.display = 'none';

      try {
        const enc = new TextEncoder();
        await T.apiJson('/upload/delete', {
          key:               uploadKey,
          deletion_password: T.toHex(enc.encode(pw)),
        });

        $('delete-form').style.display = 'none';
        $('success-box').style.display = 'flex';
      } catch (e) {
        btn.disabled = false;
        btn.innerHTML = TRASH_ICON;
        const err = $('error-box');
        err.textContent = e.message || String(e);
        err.style.display = '';
      }
    }

    // ── Init: wire DOM events + handle a missing key ──────────────────────────
    async function init() {
      const u = await T.checkAuth();
      T.updateTopbar(u);

      if (!uploadKey) {
        $('no-key-box').style.display = '';
        $('delete-form').style.display = 'none';
      }

      $('deletion-pw').addEventListener('input', checkDeleteReady);
      $('deletion-pw').addEventListener('keydown', (e) => { if (e.key === 'Enter') doDelete(); });
      $('delete-btn').addEventListener('click', doDelete);
    }

    return {
      init,
      doDelete,
      checkDeleteReady,
      get uploadKey() { return uploadKey; },
    };
  }

  // ── Browser bootstrap ───────────────────────────────────────────────────────
  if (typeof window !== 'undefined') {
    const page = createDeletePage(window);
    window.TransferDelete = page;
    page.init();
  }

  if (typeof module !== 'undefined' && module.exports) {
    module.exports = { createDeletePage };
  }
})();
