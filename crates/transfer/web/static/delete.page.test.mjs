// Tests for delete.page.js — the revoke-and-delete controller.
// Run with: node --test static/delete.page.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { makeWindow, fakeTransfer, spy } from './page-test-utils.mjs';

const require = createRequire(import.meta.url);
const { createDeletePage } = require('./delete.page.js');

const BASE_URL = 'https://transfer.test/transfer/delete';
const withKey = (key = 'KEY1') => `${BASE_URL}?key=${key}`;

// Build a page bound to a fresh jsdom window. Returns { win, page, T, doc }.
function setup({ url = withKey(), transfer = {} } = {}) {
  // fakeTransfer has no checkAuth/updateTopbar; init() needs them, so default
  // to no-op spies unless a test overrides them to assert.
  const T = fakeTransfer({
    checkAuth: async () => null,
    updateTopbar: () => {},
    ...transfer,
  });
  const win = makeWindow('delete.html', { url, Transfer: T });
  const page = createDeletePage(win);
  return { win, page, T, doc: win.document };
}

// ── URL params ────────────────────────────────────────────────────────────

test('reads key from the query string', () => {
  const { page } = setup({ url: withKey('ABC') });
  assert.equal(page.uploadKey, 'ABC');
});

test('uploadKey is null when key is absent', () => {
  const { page } = setup({ url: BASE_URL });
  assert.equal(page.uploadKey, null);
});

// ── init ────────────────────────────────────────────────────────────────────

test('init with no key reveals the no-key box and hides the form', async () => {
  const { page, doc } = setup({ url: BASE_URL });
  await page.init();
  assert.equal(doc.getElementById('no-key-box').style.display, '');
  assert.equal(doc.getElementById('delete-form').style.display, 'none');
});

test('init with a key leaves the form visible', async () => {
  const { page, doc } = setup();
  await page.init();
  assert.equal(doc.getElementById('no-key-box').style.display, 'none');
  assert.notEqual(doc.getElementById('delete-form').style.display, 'none');
});

test('init checks auth and updates the topbar', async () => {
  const checkAuth = spy(Promise.resolve({ user: 'me' }));
  const updateTopbar = spy();
  const { page } = setup({ transfer: { checkAuth, updateTopbar } });
  await page.init();
  assert.equal(checkAuth.calls.length, 1);
  assert.deepEqual(updateTopbar.calls[0], [{ user: 'me' }]);
});

test('init wires the input event so the button enables when the field is filled', async () => {
  const { page, doc, win } = setup();
  await page.init();
  const pw = doc.getElementById('deletion-pw');
  assert.equal(doc.getElementById('delete-btn').disabled, true, 'starts disabled');

  pw.value = 'secret';
  pw.dispatchEvent(new win.Event('input'));
  assert.equal(doc.getElementById('delete-btn').disabled, false, 'enabled once filled');
});

test('init wires the delete button click to doDelete', async () => {
  const apiJson = spy(Promise.resolve({}));
  const { page, doc, win } = setup({ transfer: { apiJson } });
  await page.init();
  doc.getElementById('deletion-pw').value = 'secret';
  doc.getElementById('delete-btn').dispatchEvent(new win.Event('click'));
  await Promise.resolve();
  assert.equal(apiJson.calls.length, 1);
});

// ── checkDeleteReady ──────────────────────────────────────────────────────────

test('checkDeleteReady reflects field state on the button', () => {
  const { page, doc } = setup();
  doc.getElementById('deletion-pw').value = '';
  page.checkDeleteReady();
  assert.equal(doc.getElementById('delete-btn').disabled, true);

  doc.getElementById('deletion-pw').value = 'x';
  page.checkDeleteReady();
  assert.equal(doc.getElementById('delete-btn').disabled, false);
});

test('checkDeleteReady treats whitespace-only input as empty', () => {
  const { page, doc } = setup();
  doc.getElementById('deletion-pw').value = '   ';
  page.checkDeleteReady();
  assert.equal(doc.getElementById('delete-btn').disabled, true);
});

// ── doDelete: guard ───────────────────────────────────────────────────────────

test('doDelete returns early when the password is blank', async () => {
  const apiJson = spy(Promise.resolve({}));
  const { page, doc } = setup({ transfer: { apiJson } });
  doc.getElementById('deletion-pw').value = '   ';
  await page.doDelete();
  assert.equal(apiJson.calls.length, 0);
});

// ── doDelete: happy path ──────────────────────────────────────────────────────

test('doDelete posts the hex-encoded password and shows the success box', async () => {
  const apiJson = spy(Promise.resolve({}));
  const { page, doc } = setup({ url: withKey('K9'), transfer: { apiJson } });
  doc.getElementById('deletion-pw').value = 'hunter2';

  await page.doDelete();

  assert.equal(apiJson.calls.length, 1);
  const [path, body] = apiJson.calls[0];
  assert.equal(path, '/upload/delete');
  assert.equal(body.key, 'K9');
  // 'hunter2' UTF-8 bytes hex-encoded via fakeTransfer.toHex
  assert.equal(body.deletion_password, '68756e74657232');

  assert.equal(doc.getElementById('delete-form').style.display, 'none');
  assert.equal(doc.getElementById('success-box').style.display, 'flex');
});

// ── doDelete: error path ──────────────────────────────────────────────────────

test('doDelete surfaces the error and re-enables the button', async () => {
  const apiJson = spy(() => Promise.reject(new Error('wrong deletion password')));
  const { page, doc } = setup({ transfer: { apiJson } });
  doc.getElementById('deletion-pw').value = 'nope';

  await page.doDelete();

  const err = doc.getElementById('error-box');
  assert.equal(err.textContent, 'wrong deletion password');
  assert.equal(err.style.display, '');

  const btn = doc.getElementById('delete-btn');
  assert.equal(btn.disabled, false, 'button re-enabled');
  assert.match(btn.innerHTML, /Delete transfer/, 'trash-icon label restored');

  // form stays visible, success stays hidden
  assert.notEqual(doc.getElementById('delete-form').style.display, 'none');
  assert.equal(doc.getElementById('success-box').style.display, 'none');
});
