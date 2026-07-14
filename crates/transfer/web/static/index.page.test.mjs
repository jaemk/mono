// Tests for index.page.js — the upload / encrypt / share controller.
// Run with: node --test static/index.page.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { makeWindow, fakeTransfer, fakeLocation, spy } from './page-test-utils.mjs';

const require = createRequire(import.meta.url);
const { createIndexPage } = require('./index.page.js');

// A fake File: only the bits the controller touches.
function fakeFile(name = 'photo.png', size = 2048, bytes = new Uint8Array([1, 2, 3])) {
  return { name, size, arrayBuffer: async () => bytes.buffer };
}

function setup({ url = 'https://transfer.test/transfer', transfer = {} } = {}) {
  const T = fakeTransfer({
    checkAuth: spy(Promise.resolve(null)),
    updateTopbar: spy(),
    apiGet: spy(Promise.resolve({ upload_limit_bytes: 5_000_000 })),
    apiJson: spy(Promise.resolve({ key: 'UPKEY' })),
    apiUploadBytes: spy(Promise.resolve({})),
    sha256: spy(Promise.resolve(new Uint8Array([0xaa]))),
    encryptFile: spy(Promise.resolve({ nonce: new Uint8Array([1, 2]), ciphertext: new Uint8Array(10) })),
    encryptFilename: spy(Promise.resolve(new Uint8Array([9, 9]))),
    toggleReveal: spy(),
    ...transfer,
  });
  const win = makeWindow('index.html', { url, Transfer: T, location: fakeLocation({ origin: 'https://transfer.test' }) });
  const page = createIndexPage(win);
  return { win, page, T, doc: win.document };
}

// ── init ──────────────────────────────────────────────────────────────────

test('init checks auth, seeds access password, renders stepper, shows upload limit', async () => {
  const { page, doc, T } = setup();
  await page.init();
  assert.equal(T.checkAuth.calls.length, 1);
  assert.equal(T.updateTopbar.calls.length, 1);
  assert.equal(doc.getElementById('access-pw').value, 'A'.repeat(32));
  assert.match(doc.getElementById('stepper').innerHTML, /Select/);
  assert.equal(doc.getElementById('limit-display').textContent, '5000000 B');
});

test('init tolerates a failing defaults request', async () => {
  const { page, doc } = setup({ transfer: { apiGet: spy(() => Promise.reject(new Error('down'))) } });
  await page.init();
  // still seeds the access password and renders the stepper
  assert.equal(doc.getElementById('access-pw').value, 'A'.repeat(32));
  assert.match(doc.getElementById('stepper').innerHTML, /Share/);
});

test('init wires segmented controls to update lifespan and download limit', async () => {
  const { page, doc, win } = setup();
  await page.init();
  const click = (el) => el.dispatchEvent(new win.Event('click'));

  doc.querySelector('#expire-seg button[data-val="3600"]').dispatchEvent(new win.MouseEvent('click'));
  assert.equal(page.lifespan, 3600);

  doc.querySelector('#limit-seg button[data-val=""]').dispatchEvent(new win.MouseEvent('click'));
  assert.equal(page.downloadLimit, null, 'empty data-val means unlimited');

  doc.querySelector('#limit-seg button[data-val="10"]').dispatchEvent(new win.MouseEvent('click'));
  assert.equal(page.downloadLimit, 10);
  void click;
});

test('init wires file input change to select the file', async () => {
  const { page, doc, win } = setup();
  await page.init();
  const input = doc.getElementById('file-input');
  Object.defineProperty(input, 'files', { value: [fakeFile('a.zip', 10)], configurable: true });
  input.dispatchEvent(new win.Event('change'));
  assert.equal(page.selectedFile.name, 'a.zip');
  assert.equal(page.stage, 'configure');
});

test('init wires enc-pw input to toggle the upload button', async () => {
  const { page, doc, win } = setup();
  await page.init();
  const encPw = doc.getElementById('enc-pw');
  encPw.value = 'hunter2';
  encPw.dispatchEvent(new win.Event('input'));
  assert.equal(doc.getElementById('upload-btn').disabled, false);
  encPw.value = '';
  encPw.dispatchEvent(new win.Event('input'));
  assert.equal(doc.getElementById('upload-btn').disabled, true);
});

// ── setupSeg ────────────────────────────────────────────────────────────────

test('setupSeg marks the clicked button active and reports its value', () => {
  const { page, doc, win } = setup();
  let got = null;
  page.setupSeg('expire-seg', v => { got = v; });
  const btn = doc.querySelector('#expire-seg button[data-val="86400"]');
  btn.dispatchEvent(new win.MouseEvent('click'));
  assert.equal(got, '86400');
  assert.ok(btn.classList.contains('is-active'));
  // previously-active 7d button is no longer active
  assert.ok(!doc.querySelector('#expire-seg button[data-val="604800"]').classList.contains('is-active'));
});

// ── selectFile ────────────────────────────────────────────────────────────────

test('selectFile renders the file card and advances to configure', () => {
  const { page, doc } = setup();
  page.selectFile(fakeFile('report.pdf', 1234));
  assert.equal(doc.getElementById('file-icon').textContent, 'PDF');
  assert.match(doc.getElementById('file-icon').style.color, /oklch/);
  assert.equal(doc.getElementById('file-name').textContent, 'report.pdf');
  assert.equal(doc.getElementById('file-size').textContent, '1234 B');
  assert.equal(page.stage, 'configure');
});

test('selectFile uses a fallback color for unknown extensions', () => {
  const { page, doc } = setup();
  page.selectFile(fakeFile('mystery.qux', 1));
  assert.match(doc.getElementById('file-icon').style.color, /var\(--fg-soft\)/);
});

// ── onDrop / onFileInputChange ────────────────────────────────────────────────

test('onDrop selects the dropped file and prevents default', () => {
  const { page } = setup();
  let prevented = false;
  page.onDrop({ preventDefault: () => { prevented = true; }, dataTransfer: { files: [fakeFile('x.txt', 5)] } });
  assert.ok(prevented);
  assert.equal(page.selectedFile.name, 'x.txt');
});

test('onFileInputChange ignores an empty file list', () => {
  const { page } = setup();
  page.onFileInputChange({ files: [] });
  assert.equal(page.selectedFile, null);
});

// ── onEncPwInput ──────────────────────────────────────────────────────────────

test('onEncPwInput enables the upload button only for non-blank input', () => {
  const { page, doc } = setup();
  page.onEncPwInput('  ');
  assert.equal(doc.getElementById('upload-btn').disabled, true);
  page.onEncPwInput('secret');
  assert.equal(doc.getElementById('upload-btn').disabled, false);
});

// ── showStage ───────────────────────────────────────────────────────────────

test('showStage toggles stages and sets the action button label', () => {
  const { page, doc } = setup();
  page.showStage('configure');
  assert.equal(doc.getElementById('stage-configure').style.display, 'flex');
  assert.equal(doc.getElementById('stage-idle').style.display, 'none');
  assert.match(doc.getElementById('action-btn').innerHTML, /start over/);

  page.showStage('done');
  assert.match(doc.getElementById('action-btn').innerHTML, /new transfer/);

  page.showStage('idle');
  assert.equal(doc.getElementById('action-btn').style.display, 'none');
});

// ── reset ─────────────────────────────────────────────────────────────────────

test('reset clears inputs, regenerates the access password, returns to idle', () => {
  const { page, doc } = setup();
  doc.getElementById('enc-pw').value = 'x';
  doc.getElementById('deletion-pw').value = 'y';
  page.selectFile(fakeFile());
  page.reset();
  assert.equal(page.stage, 'idle');
  assert.equal(page.selectedFile, null);
  assert.equal(doc.getElementById('enc-pw').value, '');
  assert.equal(doc.getElementById('deletion-pw').value, '');
  assert.equal(doc.getElementById('access-pw').value, 'A'.repeat(32));
  assert.equal(doc.getElementById('upload-btn').disabled, true);
});

test('handleActionBtn resets from any stage', () => {
  const { page } = setup();
  page.showStage('done');
  page.handleActionBtn();
  assert.equal(page.stage, 'idle');
});

// ── renderStepper ─────────────────────────────────────────────────────────────

test('renderStepper marks the current stage active and prior stages done', () => {
  const { page, doc } = setup();
  page.showStage('encrypting'); // index 2
  const html = doc.getElementById('stepper').innerHTML;
  assert.match(html, /Encrypt/);
  // two completed steps render a checkmark svg
  assert.ok((html.match(/polyline/g) || []).length >= 2);
});

// ── progress ────────────────────────────────────────────────────────────────

test('setProgress / setProgressRaw update pct, bar, status and bytes', () => {
  const { page, doc } = setup();
  page.setProgress(50, 'Half', '5 KB');
  assert.equal(doc.getElementById('enc-pct').textContent, '50%');
  assert.equal(doc.getElementById('enc-bar').style.width, '50%');
  assert.equal(doc.getElementById('enc-status').textContent, 'Half');
  assert.equal(doc.getElementById('enc-bytes').textContent, '5 KB');
});

test('setProgressRaw rounds the percentage for display', () => {
  const { page, doc } = setup();
  page.setProgressRaw(62.7, '', undefined);
  assert.equal(doc.getElementById('enc-pct').textContent, '63%');
});

// ── hex stream ────────────────────────────────────────────────────────────────

test('startHexStream schedules a frame; stopHexStream cancels it', () => {
  const { page, win } = setup();
  let cancelled = null;
  win.requestAnimationFrame = () => 42;
  win.cancelAnimationFrame = (id) => { cancelled = id; };
  page.startHexStream();
  page.stopHexStream();
  assert.equal(cancelled, 42);
});

// ── copy / reveal ─────────────────────────────────────────────────────────────

test('copyField shows a copied label then restores it', () => {
  const { page, doc, win } = setup();
  let timeoutCb = null;
  win.setTimeout = (fn) => { timeoutCb = fn; return 1; };
  doc.getElementById('done-link-val').textContent = 'https://x/y';
  const btn = doc.getElementById('copy-link-btn');
  btn.innerHTML = 'copy';
  page.copyField('copy-link-btn', 'done-link-val');
  assert.match(btn.innerHTML, /copied!/);
  timeoutCb();
  assert.equal(btn.innerHTML, 'copy');
});

test('copyAccessPw copies the configure-stage access password with a copied label', () => {
  const { page, doc, win } = setup();
  let timeoutCb = null;
  win.setTimeout = (fn) => { timeoutCb = fn; return 1; };
  doc.getElementById('access-pw').value = 'the-access-pw';
  const btn = doc.getElementById('copy-access-cfg-btn');
  btn.innerHTML = 'copy';
  page.copyAccessPw();
  assert.match(btn.innerHTML, /copied!/);
  timeoutCb();
  assert.equal(btn.innerHTML, 'copy');
});

test('toggleReveal delegates to Transfer.toggleReveal with the elements', () => {
  const { page, doc, T } = setup();
  page.toggleReveal('done-enc-pw', 'reveal-enc-btn');
  assert.equal(T.toggleReveal.calls.length, 1);
  assert.equal(T.toggleReveal.calls[0][0], doc.getElementById('done-enc-pw'));
  assert.equal(T.toggleReveal.calls[0][1], doc.getElementById('reveal-enc-btn'));
});

// ── doUpload: happy path ──────────────────────────────────────────────────────

test('doUpload runs encrypt→register→upload and shows the share stage', async () => {
  const { page, doc, T } = setup();
  page.selectFile(fakeFile('secret.pdf', 2048));
  doc.getElementById('enc-pw').value = 'enc-pass';
  doc.getElementById('access-pw').value = 'access-pass';

  await page.doUpload();

  assert.equal(T.encryptFile.calls.length, 1);
  assert.equal(T.apiJson.calls[0][0], '/upload/init');
  assert.equal(T.apiUploadBytes.calls[0][0], 'UPKEY');
  assert.equal(page.stage, 'done');
  assert.equal(doc.getElementById('done-filename').textContent, 'secret.pdf');
  const link = doc.getElementById('done-link').textContent;
  assert.match(link, /^https:\/\/transfer\.test\/transfer\/download\?key=UPKEY&fn=/);
});

test('doUpload includes a delete URL only when a deletion password is set', async () => {
  const { page, doc } = setup();
  page.selectFile(fakeFile());
  doc.getElementById('enc-pw').value = 'e';
  doc.getElementById('access-pw').value = 'a';
  doc.getElementById('deletion-pw').value = 'del-pass';

  await page.doUpload();

  assert.equal(doc.getElementById('delete-card').style.display, 'flex');
  assert.match(doc.getElementById('done-delete-link').textContent, /\/transfer\/delete\?key=UPKEY/);
});

test('doUpload returns early when no file is selected', async () => {
  const { page, doc, T } = setup();
  doc.getElementById('enc-pw').value = 'e';
  await page.doUpload();
  assert.equal(T.encryptFile.calls.length, 0);
});

test('doUpload returns early when the encryption password is blank', async () => {
  const { page, doc, T } = setup();
  page.selectFile(fakeFile());
  doc.getElementById('enc-pw').value = '   ';
  await page.doUpload();
  assert.equal(T.apiJson.calls.length, 0);
});

// ── doUpload: error path ──────────────────────────────────────────────────────

test('doUpload surfaces an error and returns to configure', async () => {
  const { page, doc } = setup({ transfer: { encryptFile: spy(() => Promise.reject(new Error('crypto failed'))) } });
  page.selectFile(fakeFile());
  doc.getElementById('enc-pw').value = 'e';
  doc.getElementById('access-pw').value = 'a';

  await page.doUpload();

  assert.equal(page.stage, 'configure');
  assert.equal(doc.getElementById('error-box').textContent, 'crypto failed');
  assert.equal(doc.getElementById('error-box').style.display, '');
});

// ── showDone hides the delete card when there is no delete URL ─────────────────

test('showDone without a delete URL keeps the delete card hidden', () => {
  const { page, doc } = setup();
  page.selectFile(fakeFile('a.txt', 5));
  page.showDone({ downloadUrl: 'https://x/dl', deleteUrl: null, encPw: 'e', accessPw: 'a' });
  assert.equal(doc.getElementById('delete-card').style.display, 'none');
  assert.equal(doc.getElementById('done-enc-pw').value, 'e');
  assert.equal(doc.getElementById('done-access-pw').value, 'a');
});
