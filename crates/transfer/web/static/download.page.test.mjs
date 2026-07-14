// Tests for download.page.js — the decrypt-and-download controller.
// Run with: node --test static/download.page.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { makeWindow, fakeTransfer, spy } from './page-test-utils.mjs';

const require = createRequire(import.meta.url);
const { createDownloadPage } = require('./download.page.js');

const BASE_URL = 'https://transfer.test/transfer/download';
const withKey = (key = 'KEY1', fn = '') =>
  `${BASE_URL}?key=${key}${fn ? '&fn=' + fn : ''}`;

// Build a page bound to a fresh jsdom window. Returns { win, page, T }.
function setup({ url = withKey(), transfer = {}, fetch, crypto } = {}) {
  const T = fakeTransfer(transfer);
  const win = makeWindow('download.html', { url, Transfer: T, fetch, crypto });
  const page = createDownloadPage(win);
  return { win, page, T, doc: win.document };
}

// ── URL params ────────────────────────────────────────────────────────────

test('reads key and fn from the query string', () => {
  const { page } = setup({ url: withKey('ABC', 'DEAD') });
  assert.equal(page.uploadKey, 'ABC');
  assert.equal(page.encFnHex, 'DEAD');
});

test('uploadKey is null when key is absent', () => {
  const { page } = setup({ url: BASE_URL });
  assert.equal(page.uploadKey, null);
});

// ── init ────────────────────────────────────────────────────────────────────

test('init with no key reveals the no-key box', () => {
  const { page, doc } = setup({ url: BASE_URL });
  page.init();
  assert.equal(doc.getElementById('no-key-box').style.display, '');
});

test('init with a key opens the locked stage', () => {
  const { page, doc } = setup();
  page.init();
  assert.equal(doc.getElementById('stage-locked').style.display, '');
  assert.equal(doc.getElementById('stage-decrypting').style.display, 'none');
  assert.equal(doc.getElementById('stage-ready').style.display, 'none');
});

test('init wires input events so the decrypt button enables when both fields are filled', () => {
  const { page, doc, win } = setup();
  page.init();
  const accessPw = doc.getElementById('access-pw');
  const encPw = doc.getElementById('enc-pw');
  assert.equal(doc.getElementById('decrypt-btn').disabled, true, 'starts disabled');

  accessPw.value = 'access';
  accessPw.dispatchEvent(new win.Event('input'));
  assert.equal(doc.getElementById('decrypt-btn').disabled, true, 'still disabled with one field');

  encPw.value = 'enc';
  encPw.dispatchEvent(new win.Event('input'));
  assert.equal(doc.getElementById('decrypt-btn').disabled, false, 'enabled with both fields');
});

// ── showStage ───────────────────────────────────────────────────────────────

test('showStage toggles exactly one stage; ready uses flex', () => {
  const { page, doc } = setup();
  page.showStage('ready');
  assert.equal(doc.getElementById('stage-ready').style.display, 'flex');
  assert.equal(doc.getElementById('stage-locked').style.display, 'none');
  assert.equal(doc.getElementById('stage-decrypting').style.display, 'none');
});

test('showStage hides the error box', () => {
  const { page, doc } = setup();
  doc.getElementById('error-box').style.display = '';
  page.showStage('locked');
  assert.equal(doc.getElementById('error-box').style.display, 'none');
});

// ── checkDecryptReady ─────────────────────────────────────────────────────────

test('checkDecryptReady reflects field state on the button', () => {
  const { page, doc } = setup();
  doc.getElementById('access-pw').value = '';
  doc.getElementById('enc-pw').value = 'x';
  page.checkDecryptReady();
  assert.equal(doc.getElementById('decrypt-btn').disabled, true);

  doc.getElementById('access-pw').value = 'y';
  page.checkDecryptReady();
  assert.equal(doc.getElementById('decrypt-btn').disabled, false);
});

// ── setPct ────────────────────────────────────────────────────────────────────

test('setPct updates percentage, bar width, and status text', () => {
  const { page, doc } = setup();
  page.setPct(42, 'Working…');
  assert.equal(doc.getElementById('dec-pct').textContent, '42%');
  assert.equal(doc.getElementById('dec-bar').style.width, '42%');
  assert.equal(doc.getElementById('dec-status').textContent, 'Working…');
});

test('setPct without a status leaves the status text unchanged', () => {
  const { page, doc } = setup();
  doc.getElementById('dec-status').textContent = 'previous';
  page.setPct(10);
  assert.equal(doc.getElementById('dec-status').textContent, 'previous');
});

// ── showReady ───────────────────────────────────────────────────────────────

test('showReady renders filename, sizes, and a known extension color', () => {
  const { page, doc } = setup();
  page.showReady(new Uint8Array(3), 'report.pdf', 99);
  assert.equal(doc.getElementById('stage-ready').style.display, 'flex');
  assert.equal(doc.getElementById('ready-filename').textContent, 'report.pdf');
  assert.equal(doc.getElementById('ready-icon').textContent, 'PDF');
  assert.match(doc.getElementById('ready-icon').style.color, /oklch/);
  assert.match(doc.getElementById('ready-meta').textContent, /Encrypted was 99 B/);
});

test('showReady falls back to a default color for unknown extensions', () => {
  const { page, doc } = setup();
  page.showReady(new Uint8Array(1), 'notes.xyz', 10);
  assert.equal(doc.getElementById('ready-icon').textContent, 'XYZ');
  assert.match(doc.getElementById('ready-icon').style.color, /var\(--fg-soft\)/);
});

// ── triggerDownload ───────────────────────────────────────────────────────────

test('triggerDownload is a no-op before anything is decrypted', () => {
  const { page, win } = setup();
  win.URL.createObjectURL = spy('blob:x');
  page.triggerDownload();
  assert.equal(win.URL.createObjectURL.calls.length, 0);
});

// ── decryptAndDownload: happy path ────────────────────────────────────────────

test('decryptAndDownload runs the full flow and shows the ready stage', async () => {
  const apiJson = spy((path) => {
    if (path === '/download/init') {
      return Promise.resolve({ size: 1234, download_key: 'DLK', nonce: '00', confirm_key: 'CK' });
    }
    return Promise.resolve({});
  });
  const fetch = spy({
    ok: true,
    arrayBuffer: async () => new Uint8Array([9, 9, 9]).buffer,
  });
  const fakePlain = new Uint8Array([1, 2, 3, 4]);
  const { page, doc } = setup({
    url: withKey('K9', 'AABB'),
    fetch,
    crypto: { subtle: { digest: async () => new Uint8Array([0]).buffer } },
    transfer: {
      apiJson,
      decryptFile: spy(Promise.resolve(fakePlain)),
      decryptFilename: spy(Promise.resolve('secret.pdf')),
    },
  });

  doc.getElementById('access-pw').value = 'access-pw';
  doc.getElementById('enc-pw').value = 'enc-pw';

  await page.decryptAndDownload();

  // init + confirm both POST through apiJson
  assert.deepEqual(apiJson.calls.map(c => c[0]), ['/download/init', '/download/confirm']);
  assert.equal(fetch.calls.length, 1, 'one fetch for the encrypted bytes');
  assert.equal(doc.getElementById('stage-ready').style.display, 'flex');
  assert.equal(doc.getElementById('ready-filename').textContent, 'secret.pdf');
  assert.equal(page.decryptedFilename, 'secret.pdf');
});

test('decryptAndDownload returns early when a password field is blank', async () => {
  const apiJson = spy(Promise.resolve({}));
  const { page, doc } = setup({ transfer: { apiJson } });
  doc.getElementById('access-pw').value = '   ';
  doc.getElementById('enc-pw').value = 'enc';
  await page.decryptAndDownload();
  assert.equal(apiJson.calls.length, 0);
});

// ── decryptAndDownload: error path ────────────────────────────────────────────

test('decryptAndDownload surfaces an error and returns to the locked stage', async () => {
  const apiJson = spy(() => Promise.reject(new Error('wrong access password')));
  const { page, doc } = setup({ transfer: { apiJson } });
  doc.getElementById('access-pw').value = 'a';
  doc.getElementById('enc-pw').value = 'b';

  await page.decryptAndDownload();

  assert.equal(doc.getElementById('stage-locked').style.display, '');
  assert.equal(doc.getElementById('error-box').style.display, '');
  assert.equal(doc.getElementById('error-box').textContent, 'wrong access password');
});

test('decryptAndDownload throws the server error when the download fetch is not ok', async () => {
  const apiJson = spy(() => Promise.resolve({ size: 1, download_key: 'D', nonce: '00', confirm_key: 'C' }));
  const fetch = spy({ ok: false, statusText: 'Gone', json: async () => ({ error: 'expired' }) });
  const { page, doc } = setup({ transfer: { apiJson }, fetch });
  doc.getElementById('access-pw').value = 'a';
  doc.getElementById('enc-pw').value = 'b';

  await page.decryptAndDownload();

  assert.equal(doc.getElementById('error-box').textContent, 'expired');
  assert.equal(doc.getElementById('stage-locked').style.display, '');
});

// ── hex stream ────────────────────────────────────────────────────────────────

test('startHexStream schedules a frame; stopHexStream cancels it', () => {
  let scheduled = 0;
  let cancelled = 0;
  const { page, win } = setup({});
  win.requestAnimationFrame = () => { scheduled++; return 7; };
  win.cancelAnimationFrame = (id) => { cancelled = id; };
  page.startHexStream();
  assert.equal(scheduled, 1);
  page.stopHexStream();
  assert.equal(cancelled, 7);
});

test('hex stream step renders chunk lines into the container', () => {
  // Drive exactly one animation frame by capturing the scheduled callback.
  let cb = null;
  const { page, doc, win } = setup({});
  win.requestAnimationFrame = (fn) => { cb = fn; return 1; };
  page.startHexStream();
  assert.equal(typeof cb, 'function');
  cb(); // run one frame
  assert.match(doc.getElementById('hex-stream').innerHTML, /tx-hex-line/);
  assert.match(doc.getElementById('hex-stream').innerHTML, /chunk \d{4}/);
});

// ── triggerDownload: full save path after a successful decrypt ─────────────────

test('triggerDownload writes a blob anchor and marks the save button done', async () => {
  const apiJson = spy((path) =>
    path === '/download/init'
      ? Promise.resolve({ size: 4, download_key: 'DLK', nonce: '00', confirm_key: 'CK' })
      : Promise.resolve({}));
  const { page, doc, win } = setup({
    url: withKey('K1'),
    fetch: spy({ ok: true, arrayBuffer: async () => new Uint8Array([1]).buffer }),
    crypto: { subtle: { digest: async () => new Uint8Array([0]).buffer } },
    transfer: { apiJson, decryptFile: spy(Promise.resolve(new Uint8Array([7, 7, 7]))) },
  });
  win.URL = { createObjectURL: spy('blob:fake'), revokeObjectURL: spy() };
  doc.getElementById('access-pw').value = 'a';
  doc.getElementById('enc-pw').value = 'b';
  await page.decryptAndDownload();

  page.triggerDownload();

  assert.equal(win.URL.createObjectURL.calls.length, 1);
  assert.equal(win.URL.revokeObjectURL.calls.length, 1);
  const btn = doc.getElementById('save-btn');
  assert.match(btn.innerHTML, /Saved/);
  assert.equal(btn.disabled, true);
});
