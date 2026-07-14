// Page controller for index.html — the upload / encrypt / share flow.
//
// Factory createIndexPage(win) binds to a window (document, Transfer helpers,
// fetch via Transfer, requestAnimationFrame, setTimeout, navigator.clipboard,
// location). All state lives in the returned closure so unit tests build an
// independent instance against a jsdom window. Browser bootstrap creates one
// instance from the real window (window.TransferIndex) and runs init() on load.

(function () {
  'use strict';

  function createIndexPage(win) {
    const doc = win.document;
    const T = win.Transfer;
    const $ = (id) => doc.getElementById(id);

    // ── State ─────────────────────────────────────────────────────────────────
    let stage = 'idle';
    let selectedFile = null;
    let lifespan = 604800;
    let downloadLimit = 3;
    let encPwVal = '';
    let hexRaf = null;
    let hexTick = 0;
    let currentPct = 0;

    // ── Segmented controls ────────────────────────────────────────────────────
    function setupSeg(id, onChange) {
      const seg = $(id);
      seg.querySelectorAll('button').forEach(btn => {
        btn.addEventListener('click', () => {
          seg.querySelectorAll('button').forEach(b => b.classList.remove('is-active'));
          btn.classList.add('is-active');
          onChange(btn.dataset.val);
        });
      });
    }

    // ── File handling ─────────────────────────────────────────────────────────
    function onDrop(e) {
      e.preventDefault();
      $('dropzone').classList.remove('drag-over');
      const file = e.dataTransfer.files[0];
      if (file) selectFile(file);
    }

    function onFileInputChange(input) {
      if (input.files[0]) selectFile(input.files[0]);
    }

    function selectFile(file) {
      selectedFile = file;
      const ext = T.fileExtension(file.name);
      const colors = { PDF:'oklch(0.62 0.18 25)', FIG:'oklch(0.62 0.18 280)', ZIP:'oklch(0.62 0.14 75)', PNG:'oklch(0.62 0.18 145)', JPG:'oklch(0.62 0.18 145)', MP4:'oklch(0.62 0.16 220)', DOC:'oklch(0.62 0.16 230)' };
      const icon = $('file-icon');
      icon.textContent = ext;
      icon.style.color = colors[ext] || 'var(--fg-soft)';
      $('file-name').textContent = file.name;
      $('file-size').textContent = T.formatBytes(file.size);
      showStage('configure');
    }

    // ── Password validation ───────────────────────────────────────────────────
    function onEncPwInput(val) {
      encPwVal = val;
      $('upload-btn').disabled = !val.trim();
    }

    // ── Stage management ──────────────────────────────────────────────────────
    function showStage(s) {
      stage = s;
      const stages = ['idle', 'configure', 'encrypting', 'done'];
      stages.forEach(id => {
        const el = $('stage-' + id);
        const show = id === s;
        if (id === 'configure' || id === 'done') {
          el.style.display = show ? 'flex' : 'none';
        } else {
          el.style.display = show ? '' : 'none';
        }
      });

      const btn = $('action-btn');
      if (s === 'configure' || s === 'encrypting') {
        btn.style.display = '';
        btn.innerHTML = svgX(12) + ' start over';
      } else if (s === 'done') {
        btn.style.display = '';
        btn.innerHTML = svgPlus(12) + ' new transfer';
      } else {
        btn.style.display = 'none';
      }

      hideError();
      renderStepper();
    }

    function handleActionBtn() {
      if (stage === 'done') {
        reset();
      } else {
        reset();
      }
    }

    function reset() {
      stopHexStream();
      selectedFile = null;
      encPwVal = '';
      $('enc-pw').value = '';
      $('access-pw').value = T.genAccessPassword();
      $('deletion-pw').value = '';
      $('upload-btn').disabled = true;
      $('file-input').value = '';
      showStage('idle');
    }

    // ── Stepper ───────────────────────────────────────────────────────────────
    function renderStepper() {
      const steps = ['Select', 'Configure', 'Encrypt', 'Share'];
      const order = ['idle', 'configure', 'encrypting', 'done'];
      const idx = order.indexOf(stage);
      const el = $('stepper');
      el.innerHTML = steps.map((label, i) => {
        const done = i < idx;
        const active = i === idx;
        const numCls = done ? 'done' : active ? 'active' : '';
        const numInner = done
          ? '<svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>'
          : (i + 1);
        const line = i < steps.length - 1 ? '<div class="tx-stepper-line"></div>' : '';
        return `<div class="tx-stepper-step">
          <span class="tx-stepper-num ${numCls}">${numInner}</span>
          <span class="tx-stepper-label ${active ? 'active' : ''}">${label}</span>
        </div>${line}`;
      }).join('');
    }

    // ── Upload flow ───────────────────────────────────────────────────────────
    async function doUpload() {
      const encPwRaw = $('enc-pw').value;
      const accessPwRaw = $('access-pw').value;
      if (!selectedFile || !encPwRaw.trim()) return;
      const deletionPwRaw = $('deletion-pw').value;

      showStage('encrypting');
      startHexStream();

      try {
        const enc = new TextEncoder();
        const encBytes    = enc.encode(encPwRaw);
        const accessBytes = enc.encode(accessPwRaw);

        setProgress(8, 'Reading file…', '');
        const fileBytes = new Uint8Array(await selectedFile.arrayBuffer());

        setProgress(15, 'Encrypting…', T.formatBytes(selectedFile.size));
        $('enc-filename').textContent = selectedFile.name;

        const { nonce, ciphertext } = await T.encryptFile(fileBytes, encBytes);

        setProgress(38, 'Computing hashes…', T.formatBytes(ciphertext.length) + ' encrypted');
        const encFilename = await T.encryptFilename(selectedFile.name, encBytes);
        const fileNameHash = T.toHex(await T.sha256(encFilename));
        const contentHash  = T.toHex(await T.sha256(ciphertext));
        const accessHex    = T.toHex(accessBytes);

        let deletionHex = null;
        if (deletionPwRaw.trim()) deletionHex = T.toHex(enc.encode(deletionPwRaw));

        setProgress(52, 'Registering upload…', '');
        const initResp = await T.apiJson('/upload/init', {
          nonce:             T.toHex(nonce),
          file_name_hash:    fileNameHash,
          content_hash:      contentHash,
          size:              ciphertext.length,
          access_password:   accessHex,
          deletion_password: deletionHex,
          download_limit:    downloadLimit,
          lifespan,
        });

        // Fake progress while uploading (fetch has no upload progress)
        let uploadDone = false;
        setProgress(62, 'Uploading encrypted bytes…', T.formatBytes(ciphertext.length));
        const tick = () => {
          if (uploadDone) return;
          currentPct += (95 - currentPct) * 0.04;
          setProgressRaw(currentPct, 'Uploading encrypted bytes…', T.formatBytes(ciphertext.length));
          hexRaf && win.requestAnimationFrame(tick);
        };
        win.requestAnimationFrame(tick);

        await T.apiUploadBytes(initResp.key, ciphertext);
        uploadDone = true;

        setProgress(100, 'Upload complete', T.formatBytes(ciphertext.length) + ' uploaded');
        await T.sleep(500);
        stopHexStream();

        const encFnHex = T.toHex(encFilename);
        const downloadUrl = `${win.location.origin}/transfer/download?key=${initResp.key}&fn=${encFnHex}`;
        const deleteUrl   = deletionHex ? `${win.location.origin}/transfer/delete?key=${initResp.key}` : null;

        showDone({ downloadUrl, deleteUrl, encPw: encPwRaw, accessPw: accessPwRaw });
      } catch (e) {
        stopHexStream();
        showStage('configure');
        showError(e.message || String(e));
      }
    }

    function showDone({ downloadUrl, deleteUrl, encPw, accessPw }) {
      showStage('done');
      $('delete-card').style.display = 'none';

      $('done-filename').textContent = selectedFile.name;
      const lifespanStr = T.lifespanLabel(lifespan);
      const dlStr = T.downloadLimitLabel(downloadLimit);
      $('done-meta').textContent = `${T.formatBytes(selectedFile.size)} · expires in ${lifespanStr} · ${dlStr}`;

      $('done-link').textContent = downloadUrl;
      $('done-link-val').textContent = downloadUrl;

      const encPwInput = $('done-enc-pw');
      encPwInput.value = encPw;
      encPwInput.type = 'password';
      $('reveal-enc-btn').textContent = 'reveal';
      $('done-enc-pw-val').textContent = encPw;

      const accessPwInput = $('done-access-pw');
      accessPwInput.value = accessPw;
      accessPwInput.type = 'password';
      $('reveal-access-btn').textContent = 'reveal';
      $('done-access-pw-val').textContent = accessPw;

      $('done-both-val').textContent =
        `${downloadUrl}\nEncryption password: ${encPw}\nAccess password: ${accessPw}`;
      $('copy-both-btn').onclick = () => copyField('copy-both-btn', 'done-both-val', 'copy both');

      if (deleteUrl) {
        const card = $('delete-card');
        card.style.display = 'flex';
        $('done-delete-link').textContent = deleteUrl;
        $('done-delete-val').textContent = deleteUrl;
      }
    }

    // ── Progress & hex stream ─────────────────────────────────────────────────
    function setProgress(pct, status, bytes) {
      currentPct = pct;
      setProgressRaw(pct, status, bytes);
    }

    function setProgressRaw(pct, status, bytes) {
      $('enc-pct').textContent = Math.round(pct) + '%';
      $('enc-bar').style.width = pct + '%';
      if (status) $('enc-status').textContent = status;
      if (bytes !== undefined) $('enc-bytes').textContent = bytes;
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
          const hex = T.hexStream(hexTick * 7 + i * 113, 28);
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

    // ── Copy helpers ──────────────────────────────────────────────────────────
    function copyField(btnId, valId, label) {
      const val = $(valId).textContent;
      try { win.navigator.clipboard.writeText(val); } catch {}
      const btn = $(btnId);
      const orig = btn.innerHTML;
      btn.innerHTML = svgCheck(11) + (label ? ' copied!' : ' copied!');
      win.setTimeout(() => { btn.innerHTML = orig; }, 1600);
    }

    function toggleReveal(inputId, btnId) {
      T.toggleReveal($(inputId), $(btnId));
    }

    function copyAccessPw() {
      const val = $('access-pw').value;
      try { win.navigator.clipboard.writeText(val); } catch {}
      const btn = $('copy-access-cfg-btn');
      const orig = btn.innerHTML;
      btn.innerHTML = svgCheck(11) + ' copied!';
      win.setTimeout(() => { btn.innerHTML = orig; }, 1600);
    }

    // ── Inline SVG helpers ────────────────────────────────────────────────────
    function svgCheck(s) {
      return `<svg width="${s}" height="${s}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>`;
    }
    function svgX(s) {
      return `<svg width="${s}" height="${s}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>`;
    }
    function svgPlus(s) {
      return `<svg width="${s}" height="${s}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="12" y1="5" x2="12" y2="19"/><line x1="5" y1="12" x2="19" y2="12"/></svg>`;
    }

    // ── Init: auth topbar, wire DOM events, load defaults ─────────────────────
    async function init() {
      const u = await T.checkAuth();
      T.updateTopbar(u);

      setupSeg('expire-seg', v => { lifespan = parseInt(v); });
      setupSeg('limit-seg', v => { downloadLimit = v ? parseInt(v) : null; });

      const dz = $('dropzone');
      dz.addEventListener('click', e => {
        if (e.target !== $('file-input')) $('file-input').click();
      });
      dz.addEventListener('dragover', e => { e.preventDefault(); dz.classList.add('drag-over'); });
      dz.addEventListener('dragleave', () => dz.classList.remove('drag-over'));
      dz.addEventListener('drop', onDrop);

      $('file-input').addEventListener('change', e => onFileInputChange(e.target));
      $('enc-pw').addEventListener('input', e => onEncPwInput(e.target.value));
      $('action-btn').addEventListener('click', handleActionBtn);
      $('upload-btn').addEventListener('click', doUpload);
      $('copy-access-cfg-btn').addEventListener('click', copyAccessPw);
      $('copy-link-btn').addEventListener('click', () => copyField('copy-link-btn', 'done-link-val'));
      $('reveal-enc-btn').addEventListener('click', () => toggleReveal('done-enc-pw', 'reveal-enc-btn'));
      $('copy-enc-pw-btn').addEventListener('click', () => copyField('copy-enc-pw-btn', 'done-enc-pw-val'));
      $('reveal-access-btn').addEventListener('click', () => toggleReveal('done-access-pw', 'reveal-access-btn'));
      $('copy-access-pw-btn').addEventListener('click', () => copyField('copy-access-pw-btn', 'done-access-pw-val'));
      $('copy-delete-btn').addEventListener('click', () => copyField('copy-delete-btn', 'done-delete-val'));

      try {
        const d = await T.apiGet('/upload/defaults');
        $('limit-display').textContent = T.formatBytes(d.upload_limit_bytes);
      } catch {}

      $('access-pw').value = T.genAccessPassword();
      renderStepper();
    }

    return {
      init,
      setupSeg, onDrop, onFileInputChange, selectFile, onEncPwInput,
      showStage, handleActionBtn, reset, renderStepper,
      doUpload, showDone, setProgress, setProgressRaw,
      startHexStream, stopHexStream, showError, hideError,
      copyField, toggleReveal, copyAccessPw,
      // read-only accessors for assertions
      get stage() { return stage; },
      get selectedFile() { return selectedFile; },
      get lifespan() { return lifespan; },
      get downloadLimit() { return downloadLimit; },
    };
  }

  // ── Browser bootstrap ───────────────────────────────────────────────────────
  if (typeof window !== 'undefined') {
    const page = createIndexPage(window);
    window.TransferIndex = page;
    if (document.readyState === 'loading') {
      document.addEventListener('DOMContentLoaded', page.init);
    } else {
      page.init();
    }
  }

  if (typeof module !== 'undefined' && module.exports) {
    module.exports = { createIndexPage };
  }
})();
