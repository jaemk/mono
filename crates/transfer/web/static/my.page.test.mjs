// Tests for my.page.js — the "my transfers" list controller.
// Run with: node --test static/my.page.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { makeWindow, fakeTransfer, fakeLocation, spy } from './page-test-utils.mjs';

const require = createRequire(import.meta.url);
const { createMyPage } = require('./my.page.js');

// Build a page bound to a fresh jsdom window. Returns { win, page, T, doc }.
function setup({ transfer = {}, location, confirm = () => true } = {}) {
  const T = fakeTransfer({
    checkAuth: spy(Promise.resolve({ id: 'u1' })),
    updateTopbar: spy(),
    apiGet: spy(Promise.resolve([])),
    apiJson: spy(Promise.resolve({})),
    relativeTime: () => '2 hours ago',
    ...transfer,
  });
  const win = makeWindow('my.html', {
    url: 'https://transfer.test/transfer/my',
    Transfer: T,
    location: fakeLocation(location || {}),
  });
  win.confirm = typeof confirm === 'function' ? confirm : () => confirm;
  const page = createMyPage(win);
  return { win, page, T, doc: win.document };
}

const item = (over = {}) => ({
  key: 'K1',
  size: 1024,
  date_created: '2026-06-01T00:00:00Z',
  expire_date: '2026-06-10T00:00:00Z',
  download_count: 1,
  download_limit: 0,
  deleted: false,
  expired: false,
  ...over,
});

// ── init: auth ────────────────────────────────────────────────────────────────

test('init redirects to login when not authenticated', async () => {
  const { win, page, T } = setup({ transfer: { checkAuth: spy(Promise.resolve(null)) } });
  await page.init();
  assert.equal(win.location.href, '/transfer/login?redirect=/transfer/my');
  assert.equal(T.apiGet.calls.length, 0, 'does not load transfers when unauthed');
});

test('init updates the topbar and loads transfers when authenticated', async () => {
  const me = { id: 'u9' };
  const { page, T } = setup({ transfer: { checkAuth: spy(Promise.resolve(me)) } });
  await page.init();
  assert.deepEqual(T.updateTopbar.calls[0], [me]);
  assert.deepEqual(T.apiGet.calls.map(c => c[0]), ['/my/transfers']);
});

// ── loadTransfers ─────────────────────────────────────────────────────────────

test('loadTransfers with no items shows the empty state and hides loading', async () => {
  const { page, doc } = setup({ transfer: { apiGet: spy(Promise.resolve([])) } });
  await page.loadTransfers();
  assert.equal(doc.getElementById('loading').style.display, 'none');
  assert.equal(doc.getElementById('empty-state').style.display, '');
  assert.equal(doc.getElementById('transfers-wrap').style.display, 'none');
});

test('loadTransfers with items renders rows and shows the wrap', async () => {
  const { page, doc } = setup({ transfer: { apiGet: spy(Promise.resolve([item()])) } });
  await page.loadTransfers();
  assert.equal(doc.getElementById('loading').style.display, 'none');
  assert.equal(doc.getElementById('transfers-wrap').style.display, '');
  const body = doc.getElementById('transfers-body');
  assert.equal(body.querySelectorAll('tr').length, 1);
  assert.match(body.querySelector('.tx-badge').textContent, /live/);
  const delBtn = body.querySelector('[data-action="delete"]');
  assert.ok(delBtn, 'delete button exists');
  assert.equal(delBtn.dataset.key, 'K1');
});

test('loadTransfers surfaces an error and hides loading on failure', async () => {
  const { page, doc } = setup({
    transfer: { apiGet: spy(() => Promise.reject(new Error('boom'))) },
  });
  await page.loadTransfers();
  assert.equal(doc.getElementById('loading').style.display, 'none');
  assert.equal(doc.getElementById('error-box').style.display, '');
  assert.equal(doc.getElementById('error-box').textContent, 'boom');
});

// ── renderTransfers ───────────────────────────────────────────────────────────

test('renderTransfers builds the correct status badge for live/expired/deleted', () => {
  const { page, doc } = setup();
  page.renderTransfers([
    item({ key: 'A' }),
    item({ key: 'B', expired: true }),
    item({ key: 'C', deleted: true }),
  ]);
  const badges = [...doc.getElementById('transfers-body').querySelectorAll('.tx-badge')]
    .map(b => b.textContent);
  assert.deepEqual(badges, ['live', 'expired', 'deleted']);
});

test('renderTransfers omits the delete button for deleted items', () => {
  const { page, doc } = setup();
  page.renderTransfers([item({ deleted: true })]);
  assert.equal(doc.getElementById('transfers-body').querySelector('[data-action="delete"]'), null);
});

test('renderTransfers shows a download count without a limit', () => {
  const { page, doc } = setup();
  page.renderTransfers([item({ download_count: 3, download_limit: 0 })]);
  const cells = doc.getElementById('transfers-body').querySelectorAll('td');
  assert.equal(cells[4].textContent, '3');
});

test('renderTransfers shows count/limit when a limit is set', () => {
  const { page, doc } = setup();
  page.renderTransfers([item({ download_count: 2, download_limit: 5 })]);
  const cells = doc.getElementById('transfers-body').querySelectorAll('td');
  assert.equal(cells[4].textContent, '2/5');
});

// ── doDelete ──────────────────────────────────────────────────────────────────

test('doDelete does nothing when the user cancels the confirm', async () => {
  const apiJson = spy(Promise.resolve({}));
  const { page } = setup({ transfer: { apiJson }, confirm: () => false });
  await page.doDelete('K1', { disabled: false, textContent: 'delete' });
  assert.equal(apiJson.calls.length, 0);
});

test('doDelete deletes and reloads on confirm', async () => {
  const apiJson = spy(Promise.resolve({}));
  const apiGet = spy(Promise.resolve([]));
  const { page, T } = setup({ transfer: { apiJson, apiGet }, confirm: () => true });
  const btn = { disabled: false, textContent: 'delete' };
  await page.doDelete('K1', btn);
  assert.deepEqual(apiJson.calls[0], ['/my/delete', { key: 'K1' }]);
  assert.equal(apiGet.calls.length, 1, 'reloads the list');
  assert.equal(btn.disabled, true);
});

test('doDelete surfaces an error and resets the button on failure', async () => {
  const apiJson = spy(() => Promise.reject(new Error('nope')));
  const { page, doc } = setup({ transfer: { apiJson }, confirm: () => true });
  const btn = { disabled: false, textContent: 'delete' };
  await page.doDelete('K1', btn);
  assert.equal(doc.getElementById('error-box').textContent, 'nope');
  assert.equal(btn.disabled, false);
  assert.equal(btn.textContent, 'delete');
});

// ── event delegation ──────────────────────────────────────────────────────────

test('clicking a delete button triggers doDelete via delegation', async () => {
  const apiJson = spy(Promise.resolve({}));
  const confirm = spy(true);
  const { page, doc, win } = setup({ transfer: { apiJson }, confirm });
  await page.init(); // wires the delegated listener
  page.renderTransfers([item({ key: 'DELME' })]);
  const btn = doc.getElementById('transfers-body').querySelector('[data-action="delete"]');
  btn.dispatchEvent(new win.Event('click', { bubbles: true }));
  // allow the async doDelete to run its first apiJson tick
  await new Promise(r => setTimeout(r, 0));
  assert.equal(confirm.calls.length, 1);
  assert.deepEqual(apiJson.calls[0], ['/my/delete', { key: 'DELME' }]);
});

// ── showError ─────────────────────────────────────────────────────────────────

test('showError reveals the error box with the message', () => {
  const { page, doc } = setup();
  page.showError('something broke');
  assert.equal(doc.getElementById('error-box').textContent, 'something broke');
  assert.equal(doc.getElementById('error-box').style.display, '');
});
