// Tests for settings.page.js — the account info + add-password controller.
// Run with: node --test static/settings.page.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { makeWindow, fakeTransfer, fakeLocation, spy } from './page-test-utils.mjs';

const require = createRequire(import.meta.url);
const { createSettingsPage } = require('./settings.page.js');

// Build a page bound to a fresh jsdom window. Returns { win, page, T, doc, loc }.
//
// jsdom's window.location is a non-configurable getter, so it cannot be
// replaced directly (and assigning .href is swallowed as unimplemented
// navigation). To keep redirects observable we build the window normally and
// hand the factory a thin Proxy that delegates everything to the real window
// except `location`, which resolves to a mutable fakeLocation.
function setup({ transfer = {}, location } = {}) {
  const T = fakeTransfer({
    checkAuth: spy(Promise.resolve(null)),
    updateTopbar: spy(),
    apiJson: spy(Promise.resolve({})),
    ...transfer,
  });
  const realWin = makeWindow('settings.html', { Transfer: T });
  const loc = location || fakeLocation({});
  const win = new Proxy(realWin, {
    get(target, prop) { return prop === 'location' ? loc : target[prop]; },
  });
  const page = createSettingsPage(win);
  return { win, page, T, doc: realWin.document, loc };
}

const authedUser = (over = {}) => ({
  email: 'a@b.com',
  has_password: true,
  google_linked: true,
  ...over,
});

// ── init: auth redirect ───────────────────────────────────────────────────────

test('init redirects to login when not authenticated', async () => {
  const { page, loc, T } = setup({
    transfer: { checkAuth: spy(Promise.resolve(null)) },
  });
  await page.init();
  assert.equal(loc.href, '/transfer/login?redirect=/transfer/settings');
  assert.equal(T.updateTopbar.calls.length, 0, 'no topbar update when unauthed');
  assert.equal(page.currentUser, null);
});

// ── init: authed populates account fields ─────────────────────────────────────

test('init populates the three account fields and updates the topbar', async () => {
  const me = authedUser({ email: 'jane@example.com', has_password: true, google_linked: false });
  const { page, doc, T } = setup({ transfer: { checkAuth: spy(Promise.resolve(me)) } });
  await page.init();

  assert.equal(doc.getElementById('account-email').textContent, 'jane@example.com');
  assert.equal(doc.getElementById('account-pw-status').textContent, 'set');
  assert.equal(doc.getElementById('account-google-status').textContent, 'not linked');
  assert.deepEqual(T.updateTopbar.calls[0], [me]);
  assert.equal(page.currentUser, me);
});

test('init shows the add-pw-section when the account has no password', async () => {
  const me = authedUser({ has_password: false, google_linked: true });
  const { page, doc } = setup({ transfer: { checkAuth: spy(Promise.resolve(me)) } });
  await page.init();

  assert.equal(doc.getElementById('account-pw-status').textContent, 'not set');
  assert.equal(doc.getElementById('account-google-status').textContent, 'linked');
  assert.equal(doc.getElementById('add-pw-section').style.display, '');
});

test('init keeps the add-pw-section hidden when the account has a password', async () => {
  const me = authedUser({ has_password: true });
  const { page, doc } = setup({ transfer: { checkAuth: spy(Promise.resolve(me)) } });
  await page.init();
  assert.equal(doc.getElementById('add-pw-section').style.display, 'none');
});

// ── init: event wiring ────────────────────────────────────────────────────────

test('init wires the Enter key on the password field to doAddPassword', async () => {
  const apiJson = spy(Promise.resolve({}));
  const me = authedUser({ has_password: false });
  const { page, doc, win } = setup({
    transfer: { checkAuth: spy(Promise.resolve(me)), apiJson },
  });
  await page.init();

  doc.getElementById('new-password').value = 'longenough';
  const ev = new win.KeyboardEvent('keydown', { key: 'Enter' });
  doc.getElementById('new-password').dispatchEvent(ev);
  // microtask flush so the async doAddPassword reaches apiJson
  await Promise.resolve();
  assert.deepEqual(apiJson.calls.map(c => c[0]), ['/settings/add-password']);
});

test('init wires the add-pw button click to doAddPassword', async () => {
  const apiJson = spy(Promise.resolve({}));
  const me = authedUser({ has_password: false });
  const { page, doc, win } = setup({
    transfer: { checkAuth: spy(Promise.resolve(me)), apiJson },
  });
  await page.init();

  doc.getElementById('new-password').value = 'longenough';
  doc.getElementById('add-pw-btn').dispatchEvent(new win.Event('click'));
  await Promise.resolve();
  assert.deepEqual(apiJson.calls.map(c => c[0]), ['/settings/add-password']);
});

// ── doAddPassword: validation ─────────────────────────────────────────────────

test('doAddPassword rejects a short password without calling apiJson', async () => {
  const apiJson = spy(Promise.resolve({}));
  const { page, doc } = setup({ transfer: { apiJson } });
  doc.getElementById('new-password').value = 'short';
  await page.doAddPassword();

  assert.equal(apiJson.calls.length, 0);
  assert.equal(doc.getElementById('error-box').textContent, 'Password must be at least 8 characters.');
  assert.equal(doc.getElementById('error-box').style.display, '');
});

// ── doAddPassword: success ────────────────────────────────────────────────────

test('doAddPassword on success sets the status, hides the section, and shows success', async () => {
  const apiJson = spy(Promise.resolve({}));
  const { page, doc } = setup({ transfer: { apiJson } });
  doc.getElementById('add-pw-section').style.display = '';
  doc.getElementById('new-password').value = 'longenough';

  await page.doAddPassword();

  assert.deepEqual(apiJson.calls[0], ['/settings/add-password', { password: 'longenough' }]);
  assert.equal(doc.getElementById('account-pw-status').textContent, 'set');
  assert.equal(doc.getElementById('add-pw-section').style.display, 'none');
  assert.match(doc.getElementById('success-box').textContent, /Password added/);
  assert.equal(doc.getElementById('success-box').style.display, '');
});

// ── doAddPassword: error ──────────────────────────────────────────────────────

test('doAddPassword on error shows the error and resets the button', async () => {
  const apiJson = spy(() => Promise.reject(new Error('email already taken')));
  const { page, doc } = setup({ transfer: { apiJson } });
  doc.getElementById('new-password').value = 'longenough';

  await page.doAddPassword();

  assert.equal(doc.getElementById('error-box').textContent, 'email already taken');
  assert.equal(doc.getElementById('error-box').style.display, '');
  const btn = doc.getElementById('add-pw-btn');
  assert.equal(btn.disabled, false);
  assert.equal(btn.textContent, 'Save password');
});

// ── banners ───────────────────────────────────────────────────────────────────

test('showSuccess / hideSuccess toggle the success box', () => {
  const { page, doc } = setup();
  page.showSuccess('done');
  assert.equal(doc.getElementById('success-box').textContent, 'done');
  assert.equal(doc.getElementById('success-box').style.display, '');
  page.hideSuccess();
  assert.equal(doc.getElementById('success-box').style.display, 'none');
});

test('showError / hideError toggle the error box', () => {
  const { page, doc } = setup();
  page.showError('boom');
  assert.equal(doc.getElementById('error-box').textContent, 'boom');
  assert.equal(doc.getElementById('error-box').style.display, '');
  page.hideError();
  assert.equal(doc.getElementById('error-box').style.display, 'none');
});
