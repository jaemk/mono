// Page controller for download.html — the decrypt-and-download flow.
//
// Structured as a factory, createDownloadPage(win), that binds to a given
// window (its document, location, Transfer helpers, fetch, crypto, …). All
// state lives in the returned closure, so unit tests can build an independent
// instance against a jsdom window without leaking global state between cases.
//
// Browser: a single instance is created from the real `window`, exposed as
// window.TransferDownload, and init() wires DOM events on load.
// Node (tests): `createDownloadPage` is exported via CommonJS.

(function () {
  'use strict';

  function createDownloadPage(win) {
    const doc = win.document;
    const T = win.Transfer;
    const params = new URLSearchParams(win.location.search);
    const uploadKey = params.get('key');
    const encFnHex = params.get('fn');

    const $ = (id) => doc.getElementById(id);

    // ── State ───────────────────────────────────────────────────────────────
    let hexRaf = null;
    let hexTick = 0;
    let decryptedBytes = null;
    let decryptedFilename = 'download';

    // ── Stage management ──────────────────────────────────────────────────────
    function showStage(s) {
      ['locked', 'decrypting', 'ready'].forEach(id => {
        const el = $('stage-' + id);
        const show = id === s;
        el.style.display = (show && id === 'ready') ? 'flex' : show ? '' : 'none';
      });
      hideError();
    }

    function checkDecryptReady() {
      $('decrypt-btn').disabled = !T.isDecryptReady(
        $('access-pw').value,
        $('enc-pw').value
      );
    }

    // ── Decrypt & download flow ───────────────────────────────────────────────
    async function decryptAndDownload() {
      const accessPw = $('access-pw').value;
      const encPw    = $('enc-pw').value;
      if (!accessPw.trim() || !encPw.trim()) return;

      showStage('decrypting');
      startHexStream();

      try {
        const enc = new TextEncoder();
        const accessBytes = enc.encode(accessPw);
        const encBytes    = enc.encode(encPw);
        const accessHex   = T.toHex(accessBytes);

        setPct(10, 'Verifying access password…');

        const initResp = await T.apiJson('/download/init', {
          key:             uploadKey,
          access_password: accessHex,
        });

        setPct(30, 'Fetching encrypted file…');
        $('dec-label').textContent = `Downloading · ${T.formatBytes(initResp.size)}`;

        const dlRes = await win.fetch(T.BASE + '/download', {
          method:  'POST',
          headers: { 'Content-Type': 'application/json' },
          body:    JSON.stringify({ key: initResp.download_key, access_password: accessHex }),
        });
        if (!dlRes.ok) {
          const j = await dlRes.json().catch(() => ({}));
          throw new Error(j.error || dlRes.statusText);
        }

        setPct(55, 'Decrypting…');
        const ciphertext = new Uint8Array(await dlRes.arrayBuffer());
        const nonce      = T.fromHex(initResp.nonce);

        const plaintext = await T.decryptFile(ciphertext, nonce, encBytes);

        setPct(80, 'Verifying integrity…');
        const contentHash = T.toHex(new Uint8Array(await win.crypto.subtle.digest('SHA-256', ciphertext)));
        await T.apiJson('/download/confirm', {
          key:  initResp.confirm_key,
          hash: contentHash,
        });

        setPct(95, 'Decrypting filename…');
        if (encFnHex) {
          try {
            decryptedFilename = await T.decryptFilename(T.fromHex(encFnHex), encBytes);
          } catch {
            decryptedFilename = 'download';
          }
        }

        setPct(100, 'Done');
        decryptedBytes = plaintext;
        await T.sleep(400);
        stopHexStream();
        showReady(plaintext, decryptedFilename, initResp.size);
      } catch (e) {
        stopHexStream();
        showStage('locked');
        showError(e.message || String(e));
      }
    }

    function showReady(bytes, filename, encSize) {
      showStage('ready');
      const ext = T.fileExtension(filename);
      const colors = { PDF:'oklch(0.62 0.18 25)', FIG:'oklch(0.62 0.18 280)', ZIP:'oklch(0.62 0.14 75)', PNG:'oklch(0.62 0.18 145)', JPG:'oklch(0.62 0.18 145)', MP4:'oklch(0.62 0.16 220)' };
      const icon = $('ready-icon');
      icon.textContent = ext;
      icon.style.color = colors[ext] || 'var(--fg-soft)';
      $('ready-filename').textContent = filename;
      $('ready-size').textContent = T.formatBytes(bytes.length) + ' · decrypted in-browser';
      $('ready-meta').textContent = `Original: ${T.formatBytes(bytes.length)} · Encrypted was ${T.formatBytes(encSize)}`;
    }

    function triggerDownload() {
      if (!decryptedBytes) return;
      const blob = new win.Blob([decryptedBytes]);
      const url  = win.URL.createObjectURL(blob);
      const a    = doc.createElement('a');
      a.href     = url;
      a.download = decryptedFilename;
      doc.body.appendChild(a);
      a.click();
      doc.body.removeChild(a);
      win.URL.revokeObjectURL(url);

      const btn = $('save-btn');
      btn.innerHTML = `<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg> Saved`;
      btn.disabled = true;
    }

    // ── Progress & hex stream ─────────────────────────────────────────────────
    function setPct(pct, status) {
      $('dec-pct').textContent = pct + '%';
      $('dec-bar').style.width = pct + '%';
      if (status) $('dec-status').textContent = status;
    }

    function startHexStream() {
      hexTick = 0;
      function step() {
        hexTick++;
        const container = $('hex-stream');
        if (!container) return;
        const chunkBase = Math.floor(hexTick * 2.5);
        let html = '';
        for (let i = 0; i < 5; i++) {
          const chunkNum = chunkBase - (4 - i);
          if (chunkNum < 0) continue;
          const hex = T.hexStream(hexTick * 11 + i * 97, 28);
          html += `<div class="tx-hex-line${i === 4 ? ' hl' : ''}"><span class="tx-hex-chunk">chunk ${String(chunkNum).padStart(4,'0')}</span>${hex}</div>`;
        }
        container.innerHTML = html;
        hexRaf = win.requestAnimationFrame(step);
      }
      hexRaf = win.requestAnimationFrame(step);
    }

    function stopHexStream() {
      if (hexRaf) { win.cancelAnimationFrame(hexRaf); hexRaf = null; }
    }

    // ── Error ─────────────────────────────────────────────────────────────────
    function showError(msg) {
      const el = $('error-box');
      el.textContent = msg;
      el.style.display = '';
    }
    function hideError() {
      $('error-box').style.display = 'none';
    }

    // ── Init: wire DOM events + set the opening stage ─────────────────────────
    function init() {
      const onEnter = (e) => { if (e.key === 'Enter') decryptAndDownload(); };
      $('access-pw').addEventListener('input', checkDecryptReady);
      $('access-pw').addEventListener('keydown', onEnter);
      $('enc-pw').addEventListener('input', checkDecryptReady);
      $('enc-pw').addEventListener('keydown', onEnter);
      $('decrypt-btn').addEventListener('click', decryptAndDownload);
      $('save-btn').addEventListener('click', triggerDownload);
      if (!uploadKey) {
        $('no-key-box').style.display = '';
      } else {
        showStage('locked');
      }
    }

    return {
      init,
      showStage, checkDecryptReady, decryptAndDownload,
      showReady, triggerDownload, setPct,
      startHexStream, stopHexStream, showError, hideError,
      // read-only accessors for assertions
      get uploadKey() { return uploadKey; },
      get encFnHex() { return encFnHex; },
      get decryptedFilename() { return decryptedFilename; },
    };
  }

  // ── Browser bootstrap ───────────────────────────────────────────────────────
  if (typeof window !== 'undefined') {
    const page = createDownloadPage(window);
    window.TransferDownload = page;
    (async () => {
      const u = await window.Transfer.checkAuth();
      window.Transfer.updateTopbar(u);
    })();
    if (document.readyState === 'loading') {
      document.addEventListener('DOMContentLoaded', page.init);
    } else {
      page.init();
    }
  }

  if (typeof module !== 'undefined' && module.exports) {
    module.exports = { createDownloadPage };
  }
})();
