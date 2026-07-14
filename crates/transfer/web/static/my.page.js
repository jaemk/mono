// Page controller for my.html — the "my transfers" list.
//
// Structured as a factory, createMyPage(win), that binds to a given window
// (its document, location, Transfer helpers). All state lives in the returned
// closure, so unit tests can build an independent instance against a jsdom
// window without leaking global state between cases.
//
// Browser: a single instance is created from the real `window`, exposed as
// window.TransferMy, and init() loads the list on page load.
// Node (tests): `createMyPage` is exported via CommonJS.

(function () {
  'use strict';

  function createMyPage(win) {
    const doc = win.document;
    const T = win.Transfer;

    async function loadTransfers() {
      try {
        const items = await T.apiGet('/my/transfers');
        doc.getElementById('loading').style.display = 'none';
        if (!items.length) {
          doc.getElementById('empty-state').style.display = '';
          return;
        }
        renderTransfers(items);
        doc.getElementById('transfers-wrap').style.display = '';
      } catch (e) {
        doc.getElementById('loading').style.display = 'none';
        showError(e.message || 'Failed to load transfers');
      }
    }

    function renderTransfers(items) {
      const tbody = doc.getElementById('transfers-body');
      tbody.innerHTML = items.map(item => {
        const status   = item.deleted ? 'deleted' : item.expired ? 'expired' : 'live';
        const chipCls  = status === 'live' ? 'tx-status-live' : status === 'expired' ? 'tx-status-expired' : 'tx-status-deleted';
        const dlCount  = item.download_limit ? `${item.download_count}/${item.download_limit}` : String(item.download_count);
        const expiry   = new Date(item.expire_date).toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric' });
        const created  = T.relativeTime(item.date_created);
        const linkHref = `/transfer/download?key=${item.key}`;
        const canDelete = !item.deleted;
        return `<tr>
      <td><span class="tx-badge ${chipCls}">${status}</span></td>
      <td class="tx-mono" style="font-size:12px">${T.formatBytes(item.size)}</td>
      <td class="tx-meta">${created}</td>
      <td class="tx-meta">${item.deleted || item.expired ? '—' : expiry}</td>
      <td class="tx-meta tx-mono" style="font-size:12px">${dlCount}</td>
      <td>
        <div class="tx-row" style="gap:6px;justify-content:flex-end">
          ${status === 'live' ? `<a class="tx-btn is-sm is-ghost" href="${linkHref}">view</a>` : ''}
          ${canDelete ? `<button class="tx-btn is-sm is-ghost" style="color:var(--danger)" data-action="delete" data-key="${item.key}">delete</button>` : ''}
        </div>
      </td>
    </tr>`;
      }).join('');
    }

    async function doDelete(key, btn) {
      if (!win.confirm('Delete this transfer? This cannot be undone.')) return;
      btn.disabled = true; btn.textContent = '…';
      try {
        await T.apiJson('/my/delete', { key });
        await loadTransfers();
      } catch (e) {
        showError(e.message || 'Delete failed');
        btn.disabled = false; btn.textContent = 'delete';
      }
    }

    function showError(msg) {
      const e = doc.getElementById('error-box');
      e.textContent = msg;
      e.style.display = '';
    }

    async function init() {
      doc.getElementById('transfers-body').addEventListener('click', (ev) => {
        const btn = ev.target.closest('[data-action="delete"]');
        if (!btn) return;
        doDelete(btn.dataset.key, btn);
      });

      const me = await T.checkAuth();
      if (!me) { win.location.href = '/transfer/login?redirect=/transfer/my'; return; }
      T.updateTopbar(me);
      await loadTransfers();
    }

    return { init, loadTransfers, renderTransfers, doDelete, showError };
  }

  // ── Browser bootstrap ───────────────────────────────────────────────────────
  if (typeof window !== 'undefined') {
    const page = createMyPage(window);
    window.TransferMy = page;
    page.init();
  }

  if (typeof module !== 'undefined' && module.exports) {
    module.exports = { createMyPage };
  }
})();
