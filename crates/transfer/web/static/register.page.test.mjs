// Tests for register.page.js — the account-creation + email-verify controller.
// Run with: node --test static/register.page.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { makeWindow, fakeTransfer, fakeLocation, spy } from './page-test-utils.mjs';

const require = createRequire(import.meta.url);
const { createRegisterPage } = require('./register.page.js');

// Build a page bound to a fresh jsdom window. Returns { win, page, doc }.
//
// jsdom's window.location is a non-configurable own property, so makeWindow's
// opts.location path can't redefine it on the raw window. We instead hand the
// factory a thin wrapper window that delegates to jsdom but carries a fake,
// observable location (per page-test-utils.fakeLocation) — redirects assign to
// win.location.href and are recorded there. Timers are stubbed so countdown
// logic is observable without real time.
function setup({ transfer = {}, location = {} } = {}) {
  const base = makeWindow('register.html', {
    Transfer: fakeTransfer({
      checkAuth: spy(Promise.resolve(null)),
      updateTopbar: spy(),
      apiGet: spy(Promise.resolve({ email_enabled: true })),
      apiJson: spy(Promise.resolve({})),
      ...transfer,
    }),
  });
  const win = {
    document: base.document,
    Transfer: base.Transfer,
    location: fakeLocation(location),
    setInterval: spy(42),
    clearInterval: spy(),
    Event: base.Event,
  };
  const page = createRegisterPage(win);
  return { win, page, doc: win.document, T: win.Transfer };
}

// ── init ──────────────────────────────────────────────────────────────────────

test('init redirects to /transfer/my when already authenticated', async () => {
  const { page, win } = setup({ transfer: { checkAuth: spy(Promise.resolve({ id: 1 })) } });
  await page.init();
  assert.equal(win.location.href, '/transfer/my');
});

test('init updates the topbar and leaves the form when not authenticated', async () => {
  const { page, doc, T } = setup();
  await page.init();
  assert.equal(T.updateTopbar.calls.length, 1);
  assert.deepEqual(T.updateTopbar.calls[0], [null]);
  assert.doesNotMatch(doc.getElementById('stage-form').innerHTML, /not enabled/);
});

test('init replaces the form when email registration is disabled', async () => {
  const { page, doc } = setup({ transfer: { apiGet: spy(Promise.resolve({ email_enabled: false })) } });
  await page.init();
  assert.match(doc.getElementById('stage-form').innerHTML, /Email registration is not enabled/);
});

test('init wires the register button click to doRegister', async () => {
  const apiJson = spy(Promise.resolve({ pending_id: 'P1' }));
  const { page, doc, win } = setup({ transfer: { apiJson } });
  await page.init();
  doc.getElementById('reg-email').value = 'a@b.com';
  doc.getElementById('reg-password').value = 'password123';
  doc.getElementById('reg-btn').dispatchEvent(new win.Event('click'));
  await Promise.resolve();
  await Promise.resolve();
  assert.equal(apiJson.calls.length, 1);
  assert.equal(apiJson.calls[0][0], '/auth/register');
});

// ── doRegister: validation ────────────────────────────────────────────────────

test('doRegister shows an error and does not call apiJson when email is missing', async () => {
  const apiJson = spy(Promise.resolve({}));
  const { page, doc } = setup({ transfer: { apiJson } });
  doc.getElementById('reg-email').value = '   ';
  doc.getElementById('reg-password').value = 'password123';
  await page.doRegister();
  assert.equal(apiJson.calls.length, 0);
  assert.equal(doc.getElementById('error-box').style.display, '');
  assert.match(doc.getElementById('error-box').textContent, /Enter your email/);
});

test('doRegister shows an error and does not call apiJson when the password is too short', async () => {
  const apiJson = spy(Promise.resolve({}));
  const { page, doc } = setup({ transfer: { apiJson } });
  doc.getElementById('reg-email').value = 'a@b.com';
  doc.getElementById('reg-password').value = 'short';
  await page.doRegister();
  assert.equal(apiJson.calls.length, 0);
  assert.match(doc.getElementById('error-box').textContent, /at least 8 characters/);
});

// ── doRegister: success ───────────────────────────────────────────────────────

test('doRegister success swaps to the verify stage, sets pendingId, and starts the countdown', async () => {
  const apiJson = spy(Promise.resolve({ pending_id: 'PEND-9' }));
  const { page, doc, win } = setup({ transfer: { apiJson } });
  doc.getElementById('reg-email').value = 'user@example.com';
  doc.getElementById('reg-password').value = 'password123';

  await page.doRegister();

  assert.equal(apiJson.calls[0][0], '/auth/register');
  assert.deepEqual(apiJson.calls[0][1], { email: 'user@example.com', password: 'password123' });
  assert.equal(page.pendingId, 'PEND-9');
  assert.equal(doc.getElementById('stage-form').style.display, 'none');
  assert.equal(doc.getElementById('stage-verify').style.display, '');
  assert.equal(doc.getElementById('verify-email-display').textContent, 'user@example.com');
  assert.equal(doc.getElementById('page-title').textContent, 'Verify your email');
  // countdown started via the stubbed timer
  assert.equal(win.setInterval.calls.length, 1);
  assert.equal(doc.getElementById('resend-countdown').textContent, 'Resend in 60s');
});

// ── doRegister: error ─────────────────────────────────────────────────────────

test('doRegister error shows the message and re-enables the button', async () => {
  const apiJson = spy(() => Promise.reject(new Error('email already in use')));
  const { page, doc } = setup({ transfer: { apiJson } });
  doc.getElementById('reg-email').value = 'a@b.com';
  doc.getElementById('reg-password').value = 'password123';

  await page.doRegister();

  assert.equal(doc.getElementById('error-box').textContent, 'email already in use');
  assert.equal(doc.getElementById('reg-btn').disabled, false);
  assert.equal(doc.getElementById('reg-btn').textContent, 'Create account');
  assert.equal(page.pendingId, null);
});

// ── doVerify ──────────────────────────────────────────────────────────────────

test('doVerify shows an error when the code is not 6 characters', async () => {
  const apiJson = spy(Promise.resolve({}));
  const { page, doc } = setup({ transfer: { apiJson } });
  doc.getElementById('verify-code').value = '123';
  await page.doVerify();
  assert.equal(apiJson.calls.length, 0);
  assert.match(doc.getElementById('error-box').textContent, /6-digit code/);
});

test('doVerify success calls apiJson with pendingId and redirects to /transfer/my', async () => {
  const apiJson = spy((path) =>
    path === '/auth/register'
      ? Promise.resolve({ pending_id: 'P77' })
      : Promise.resolve({}));
  const { page, doc, win } = setup({ transfer: { apiJson } });
  // register first to seed pendingId
  doc.getElementById('reg-email').value = 'a@b.com';
  doc.getElementById('reg-password').value = 'password123';
  await page.doRegister();

  doc.getElementById('verify-code').value = '123456';
  await page.doVerify();

  const verifyCall = apiJson.calls.find(c => c[0] === '/auth/verify-code');
  assert.ok(verifyCall, 'verify-code endpoint hit');
  assert.deepEqual(verifyCall[1], { pending_id: 'P77', code: '123456' });
  assert.equal(win.location.href, '/transfer/my');
});

test('doVerify error shows the message and re-enables the button', async () => {
  const apiJson = spy(() => Promise.reject(new Error('invalid code')));
  const { page, doc, win } = setup({ transfer: { apiJson } });
  doc.getElementById('verify-code').value = '000000';
  await page.doVerify();
  assert.equal(doc.getElementById('error-box').textContent, 'invalid code');
  assert.equal(doc.getElementById('verify-btn').disabled, false);
  assert.equal(doc.getElementById('verify-btn').textContent, 'Verify');
  assert.notEqual(win.location.href, '/transfer/my');
});

// ── doResend ──────────────────────────────────────────────────────────────────

test('doResend returns early when there is no pendingId', async () => {
  const apiJson = spy(Promise.resolve({}));
  const { page } = setup({ transfer: { apiJson } });
  await page.doResend();
  assert.equal(apiJson.calls.length, 0);
});

test('doResend success calls resend-code and restarts the countdown', async () => {
  const apiJson = spy((path) =>
    path === '/auth/register'
      ? Promise.resolve({ pending_id: 'PR1' })
      : Promise.resolve({}));
  const { page, doc, win } = setup({ transfer: { apiJson } });
  doc.getElementById('reg-email').value = 'a@b.com';
  doc.getElementById('reg-password').value = 'password123';
  await page.doRegister();
  const before = win.setInterval.calls.length;

  await page.doResend();

  const resendCall = apiJson.calls.find(c => c[0] === '/auth/resend-code');
  assert.ok(resendCall, 'resend-code endpoint hit');
  assert.deepEqual(resendCall[1], { pending_id: 'PR1' });
  assert.equal(win.setInterval.calls.length, before + 1, 'countdown restarted');
});

test('doResend error shows the message and re-enables the resend button', async () => {
  const apiJson = spy((path) =>
    path === '/auth/register'
      ? Promise.resolve({ pending_id: 'PR2' })
      : Promise.reject(new Error('rate limited')));
  const { page, doc } = setup({ transfer: { apiJson } });
  doc.getElementById('reg-email').value = 'a@b.com';
  doc.getElementById('reg-password').value = 'password123';
  await page.doRegister();

  await page.doResend();

  assert.equal(doc.getElementById('error-box').textContent, 'rate limited');
  assert.equal(doc.getElementById('resend-btn').disabled, false);
});

// ── startResendCountdown ──────────────────────────────────────────────────────

test('startResendCountdown sets the initial countdown text and disables the button', () => {
  const { page, doc, win } = setup();
  page.startResendCountdown(30);
  assert.equal(doc.getElementById('resend-countdown').textContent, 'Resend in 30s');
  assert.equal(doc.getElementById('resend-btn').disabled, true);
  assert.equal(win.setInterval.calls.length, 1);
});

test('startResendCountdown tick clears the interval and re-enables the button at zero', () => {
  // Capture the interval callback via the stub, then drive it manually.
  let cb = null;
  const { page, doc, win } = setup();
  win.setInterval = spy((fn) => { cb = fn; return 5; });
  page.startResendCountdown(1);
  assert.equal(typeof cb, 'function');
  cb(); // 1 -> 0: should finish
  assert.equal(doc.getElementById('resend-countdown').textContent, '');
  assert.equal(doc.getElementById('resend-btn').disabled, false);
  assert.equal(win.clearInterval.calls.length >= 1, true);
});

// ── showError / hideError ─────────────────────────────────────────────────────

test('showError and hideError toggle the error box', () => {
  const { page, doc } = setup();
  page.showError('boom');
  assert.equal(doc.getElementById('error-box').textContent, 'boom');
  assert.equal(doc.getElementById('error-box').style.display, '');
  page.hideError();
  assert.equal(doc.getElementById('error-box').style.display, 'none');
});
