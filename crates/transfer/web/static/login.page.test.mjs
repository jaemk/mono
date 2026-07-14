// Tests for login.page.js — the sign-in controller (Google + email/password).
// Run with: node --test static/login.page.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { makeWindow, fakeTransfer, fakeLocation, spy } from './page-test-utils.mjs';

const require = createRequire(import.meta.url);
const { createLoginPage } = require('./login.page.js');

const BASE_URL = 'https://transfer.test/transfer/login';

// Build a page bound to a fresh jsdom window. Returns { win, page, T, doc, loc }.
//
// jsdom's window.location is non-configurable, so the page is bound to a thin
// wrapper around the jsdom window whose `location` is the observable fake (the
// factory only ever reads win.document / win.Transfer / win.location). `win` in
// the returned object is this wrapper; `dom` is the underlying jsdom window for
// constructing DOM Events.
function setup({ url = BASE_URL, search = '', transfer = {} } = {}) {
  const T = fakeTransfer(transfer);
  const loc = fakeLocation({ href: url, search });
  const dom = makeWindow('login.html', { url, Transfer: T });
  const win = new Proxy(dom, {
    get(target, prop) {
      if (prop === 'location') return loc;
      const v = target[prop];
      return typeof v === 'function' ? v.bind(target) : v;
    },
  });
  const page = createLoginPage(win);
  return { win, dom, page, T, doc: dom.document, loc };
}

// ── init: already authed ──────────────────────────────────────────────────────

test('init redirects to /transfer/my when already authed', async () => {
  const checkAuth = spy(Promise.resolve({ id: 'u1' }));
  const { page, loc } = setup({ transfer: { checkAuth } });
  await page.init();
  assert.equal(checkAuth.calls.length, 1);
  assert.equal(loc.href, '/transfer/my');
});

test('init redirects to the redirect param when already authed', async () => {
  const checkAuth = spy(Promise.resolve({ id: 'u1' }));
  const { page, loc } = setup({ search: '?redirect=/transfer/dashboard', transfer: { checkAuth } });
  await page.init();
  assert.equal(loc.href, '/transfer/dashboard');
});

// ── init: not authed ──────────────────────────────────────────────────────────

test('init calls updateTopbar(null) when not authed', async () => {
  const updateTopbar = spy();
  const apiGet = spy(Promise.resolve({}));
  const { page } = setup({ transfer: { checkAuth: spy(Promise.resolve(null)), updateTopbar, apiGet } });
  await page.init();
  assert.equal(updateTopbar.calls.length, 1);
  assert.deepEqual(updateTopbar.calls[0], [null]);
});

test('init does not redirect when not authed', async () => {
  const apiGet = spy(Promise.resolve({}));
  const { page, loc } = setup({ transfer: { checkAuth: spy(Promise.resolve(null)), updateTopbar: spy(), apiGet } });
  await page.init();
  assert.equal(loc.href, BASE_URL);
});

// ── init: defaults rendering ──────────────────────────────────────────────────

test('init reveals the google section and divider when both auth methods are enabled', async () => {
  const apiGet = spy(Promise.resolve({
    google_enabled: true, google_client_id: 'CID', email_enabled: true,
  }));
  const { page, doc } = setup({ transfer: { checkAuth: spy(Promise.resolve(null)), updateTopbar: spy(), apiGet } });
  await page.init();
  assert.equal(doc.getElementById('google-section').style.display, '');
  assert.equal(doc.getElementById('email-section').style.display, 'flex');
  assert.equal(doc.getElementById('or-divider').style.display, '');
  assert.equal(page.googleClientId, 'CID');
});

test('init reveals only the email section when google is disabled', async () => {
  const apiGet = spy(Promise.resolve({ google_enabled: false, email_enabled: true }));
  const { page, doc } = setup({ transfer: { checkAuth: spy(Promise.resolve(null)), updateTopbar: spy(), apiGet } });
  await page.init();
  assert.equal(doc.getElementById('email-section').style.display, 'flex');
  assert.equal(doc.getElementById('google-section').style.display, 'none');
  assert.equal(doc.getElementById('or-divider').style.display, 'none');
});

test('init shows an error when no auth method is enabled', async () => {
  const apiGet = spy(Promise.resolve({ google_enabled: false, email_enabled: false }));
  const { page, doc } = setup({ transfer: { checkAuth: spy(Promise.resolve(null)), updateTopbar: spy(), apiGet } });
  await page.init();
  assert.equal(doc.getElementById('error-box').style.display, '');
  assert.match(doc.getElementById('error-box').textContent, /not configured/);
});

test('init shows an error when loading defaults fails', async () => {
  const apiGet = spy(() => Promise.reject(new Error('boom')));
  const { page, doc } = setup({ transfer: { checkAuth: spy(Promise.resolve(null)), updateTopbar: spy(), apiGet } });
  await page.init();
  assert.equal(doc.getElementById('error-box').style.display, '');
  assert.match(doc.getElementById('error-box').textContent, /Failed to load config: boom/);
});

// ── init: event wiring ────────────────────────────────────────────────────────

test('init wires the login button click to doLogin', async () => {
  const apiJson = spy(Promise.resolve({}));
  const { page, doc, dom } = setup({
    transfer: { checkAuth: spy(Promise.resolve(null)), updateTopbar: spy(), apiGet: spy(Promise.resolve({ email_enabled: true })), apiJson },
  });
  await page.init();
  doc.getElementById('login-email').value = 'a@b.com';
  doc.getElementById('login-password').value = 'pw';
  doc.getElementById('login-btn').dispatchEvent(new dom.Event('click'));
  await Promise.resolve();
  assert.equal(apiJson.calls.length, 1);
  assert.equal(apiJson.calls[0][0], '/auth/login');
});

test('init wires Enter on the password field to doLogin', async () => {
  const apiJson = spy(Promise.resolve({}));
  const { page, doc, dom } = setup({
    transfer: { checkAuth: spy(Promise.resolve(null)), updateTopbar: spy(), apiGet: spy(Promise.resolve({ email_enabled: true })), apiJson },
  });
  await page.init();
  doc.getElementById('login-email').value = 'a@b.com';
  doc.getElementById('login-password').value = 'pw';
  const ev = new dom.KeyboardEvent('keydown', { key: 'Enter' });
  doc.getElementById('login-password').dispatchEvent(ev);
  await Promise.resolve();
  assert.equal(apiJson.calls.length, 1);
  assert.equal(apiJson.calls[0][0], '/auth/login');
});

// ── doLogin ───────────────────────────────────────────────────────────────────

test('doLogin posts credentials and redirects on success', async () => {
  const apiJson = spy(Promise.resolve({}));
  const { page, doc, loc } = setup({ search: '?redirect=/transfer/my', transfer: { apiJson } });
  doc.getElementById('login-email').value = '  user@example.com  ';
  doc.getElementById('login-password').value = 'secret';
  await page.doLogin();
  assert.equal(apiJson.calls.length, 1);
  assert.deepEqual(apiJson.calls[0], ['/auth/login', { email: 'user@example.com', password: 'secret' }]);
  assert.equal(loc.href, '/transfer/my');
});

test('doLogin validates empty fields and never calls apiJson', async () => {
  const apiJson = spy(Promise.resolve({}));
  const { page, doc } = setup({ transfer: { apiJson } });
  doc.getElementById('login-email').value = '';
  doc.getElementById('login-password').value = '';
  await page.doLogin();
  assert.equal(apiJson.calls.length, 0);
  assert.equal(doc.getElementById('error-box').style.display, '');
  assert.match(doc.getElementById('error-box').textContent, /Enter your email and password/);
});

test('doLogin shows an error and re-enables the button on failure', async () => {
  const apiJson = spy(() => Promise.reject(new Error('bad credentials')));
  const { page, doc, loc } = setup({ transfer: { apiJson } });
  doc.getElementById('login-email').value = 'a@b.com';
  doc.getElementById('login-password').value = 'pw';
  await page.doLogin();
  assert.equal(doc.getElementById('error-box').textContent, 'bad credentials');
  assert.equal(doc.getElementById('error-box').style.display, '');
  const btn = doc.getElementById('login-btn');
  assert.equal(btn.disabled, false);
  assert.equal(btn.textContent, 'Sign in');
  assert.equal(loc.href, BASE_URL, 'no redirect on failure');
});

// ── onGoogleCredential ────────────────────────────────────────────────────────

test('onGoogleCredential posts the id token and redirects on success', async () => {
  const apiJson = spy(Promise.resolve({}));
  const { page, loc } = setup({ search: '?redirect=/transfer/landing', transfer: { apiJson } });
  await page.onGoogleCredential({ credential: 'TOKEN' });
  assert.deepEqual(apiJson.calls[0], ['/auth/google', { id_token: 'TOKEN' }]);
  assert.equal(loc.href, '/transfer/landing');
});

test('onGoogleCredential shows an error on failure', async () => {
  const apiJson = spy(() => Promise.reject(new Error('google failed')));
  const { page, doc, loc } = setup({ transfer: { apiJson } });
  await page.onGoogleCredential({ credential: 'TOKEN' });
  assert.equal(doc.getElementById('error-box').textContent, 'google failed');
  assert.equal(doc.getElementById('error-box').style.display, '');
  assert.equal(loc.href, BASE_URL, 'no redirect on failure');
});

// ── loadGSI ───────────────────────────────────────────────────────────────────

test('loadGSI appends a script element with the GSI client src', () => {
  const { page, doc } = setup();
  page.loadGSI('CID');
  const scripts = Array.from(doc.head.querySelectorAll('script'));
  const gsi = scripts.find(s => s.src === 'https://accounts.google.com/gsi/client');
  assert.ok(gsi, 'GSI script was appended to head');
  assert.equal(gsi.async, true);
});

// ── showError / hideError ─────────────────────────────────────────────────────

test('showError displays the error box with the message', () => {
  const { page, doc } = setup();
  page.showError('something went wrong');
  assert.equal(doc.getElementById('error-box').textContent, 'something went wrong');
  assert.equal(doc.getElementById('error-box').style.display, '');
});

test('hideError hides the error box', () => {
  const { page, doc } = setup();
  doc.getElementById('error-box').style.display = '';
  page.hideError();
  assert.equal(doc.getElementById('error-box').style.display, 'none');
});
