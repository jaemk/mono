// Tests for app.js — run with: node --test static/app.test.mjs
// Node 18+ required (native WebCrypto via globalThis.crypto).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const T = require('./app.js');

const enc = new TextEncoder();
const pw1 = enc.encode('encryption-password');
const pw2 = enc.encode('access-password-random32chars!!!');

// ── Encoding helpers ──────────────────────────────────────────────────────

test('toHex / fromHex roundtrip', () => {
  const bytes = new Uint8Array([0x00, 0xab, 0xcd, 0xef, 0xff]);
  assert.equal(T.toHex(bytes), '00abcdefff');
  assert.deepEqual(T.fromHex('00abcdefff'), bytes);
});

test('toHex produces lowercase', () => {
  assert.equal(T.toHex(new Uint8Array([0xAB])), 'ab');
});

// ── Crypto: encrypt/decrypt ───────────────────────────────────────────────

test('encryptFile / decryptFile roundtrip with same password', async () => {
  const plaintext = enc.encode('hello transfer');
  const { nonce, ciphertext } = await T.encryptFile(plaintext, pw1);
  assert.equal(nonce.length, 12, 'nonce must be 12 bytes');
  assert.ok(ciphertext.length > plaintext.length, 'ciphertext includes GCM tag');
  const recovered = await T.decryptFile(ciphertext, nonce, pw1);
  assert.deepEqual(recovered, plaintext);
});

test('decryptFile with wrong password throws', async () => {
  const plaintext = enc.encode('secret data');
  const { nonce, ciphertext } = await T.encryptFile(plaintext, pw1);
  await assert.rejects(
    () => T.decryptFile(ciphertext, nonce, pw2),
    'decrypting with a different password must fail'
  );
});

test('two-password invariant: enc_pw encrypts, access_pw cannot decrypt', async () => {
  // Core invariant of the two-password flow: the file is encrypted with
  // enc_pw (never sent to server), not with the access_pw (sent to server).
  // A recipient who only presents the access_pw cannot decrypt the file.
  const fileBytes = enc.encode('confidential file contents');
  const { nonce, ciphertext } = await T.encryptFile(fileBytes, pw1); // enc with pw1
  await assert.rejects(
    () => T.decryptFile(ciphertext, nonce, pw2),               // decrypt attempt with pw2
    'access_pw must not decrypt content encrypted with enc_pw'
  );
  const recovered = await T.decryptFile(ciphertext, nonce, pw1); // correct pw
  assert.deepEqual(recovered, fileBytes);
});

test('each encryptFile call uses a fresh nonce', async () => {
  const data = enc.encode('x');
  const a = await T.encryptFile(data, pw1);
  const b = await T.encryptFile(data, pw1);
  assert.notEqual(T.toHex(a.nonce), T.toHex(b.nonce), 'nonces must differ');
  assert.notEqual(T.toHex(a.ciphertext), T.toHex(b.ciphertext), 'ciphertexts must differ');
});

// ── Crypto: filename ──────────────────────────────────────────────────────

test('encryptFilename / decryptFilename roundtrip', async () => {
  const name = 'report Q1 2026.pdf';
  const combined = await T.encryptFilename(name, pw1);
  assert.ok(combined.length > 12, 'combined must include nonce prefix');
  const recovered = await T.decryptFilename(combined, pw1);
  assert.equal(recovered, name);
});

test('decryptFilename with wrong password throws', async () => {
  const combined = await T.encryptFilename('secret.docx', pw1);
  await assert.rejects(() => T.decryptFilename(combined, pw2));
});

// ── genAccessPassword ─────────────────────────────────────────────────────

test('genAccessPassword returns 32 alphanumeric characters', () => {
  const pw = T.genAccessPassword();
  assert.equal(pw.length, 32);
  assert.match(pw, /^[A-Za-z0-9]{32}$/, 'must be alphanumeric only');
});

test('genAccessPassword produces unique values', () => {
  const samples = new Set(Array.from({ length: 20 }, () => T.genAccessPassword()));
  assert.ok(samples.size > 1, 'successive calls must not all return the same value');
});

// ── Utilities ─────────────────────────────────────────────────────────────

test('formatBytes thresholds', () => {
  assert.equal(T.formatBytes(0), '0 B');
  assert.equal(T.formatBytes(999), '999 B');
  assert.equal(T.formatBytes(1000), '1.0 KB');
  assert.equal(T.formatBytes(1_500_000), '1.5 MB');
  assert.equal(T.formatBytes(2_000_000_000), '2.0 GB');
});

test('hexStream is deterministic', () => {
  assert.equal(T.hexStream(42), T.hexStream(42));
  assert.notEqual(T.hexStream(1), T.hexStream(2));
});

test('hexStream default length is 28 visible chars (groups of 4 separated by spaces)', () => {
  const out = T.hexStream(0);
  // 28 hex chars + 6 spaces = 34 total characters (7 groups of 4, 6 spaces between)
  assert.equal(out.length, 34);
});

// ── sha256 ────────────────────────────────────────────────────────────────

test('sha256 of empty input is correct', async () => {
  // SHA-256('') = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
  const result = T.toHex(await T.sha256(new Uint8Array(0)));
  assert.equal(result, 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855');
});

test('sha256 of known string is correct', async () => {
  // SHA-256('abc') = ba7816bf8f01cfea414140de5dae2ec73b00361bbef0469348423f656b9d18b
  // (well-known test vector)
  const result = T.toHex(await T.sha256(enc.encode('abc')));
  assert.equal(result, 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad');
});

// ── toHex / fromHex edge cases ────────────────────────────────────────────

test('toHex of empty array returns empty string', () => {
  assert.equal(T.toHex(new Uint8Array(0)), '');
});

test('fromHex of empty string returns empty array', () => {
  assert.deepEqual(T.fromHex(''), new Uint8Array(0));
});

test('toHex / fromHex roundtrip for all byte values', () => {
  const all = new Uint8Array(256);
  for (let i = 0; i < 256; i++) all[i] = i;
  assert.deepEqual(T.fromHex(T.toHex(all)), all);
});

// ── Crypto edge cases ─────────────────────────────────────────────────────

test('encrypt and decrypt empty plaintext', async () => {
  const empty = new Uint8Array(0);
  const { nonce, ciphertext } = await T.encryptFile(empty, pw1);
  const recovered = await T.decryptFile(ciphertext, nonce, pw1);
  assert.deepEqual(recovered, empty);
});

test('encrypt and decrypt large plaintext (100 KB)', async () => {
  const large = new Uint8Array(100 * 1024);
  for (let i = 0; i < large.length; i++) large[i] = i & 0xff;
  const { nonce, ciphertext } = await T.encryptFile(large, pw1);
  const recovered = await T.decryptFile(ciphertext, nonce, pw1);
  assert.deepEqual(recovered, large);
});

test('encryptFilename / decryptFilename roundtrip with unicode', async () => {
  const name = '报告 2026年Q1 — résumé.pdf';
  const combined = await T.encryptFilename(name, pw1);
  const recovered = await T.decryptFilename(combined, pw1);
  assert.equal(recovered, name);
});

test('aesKey is deterministic: same password produces matching encryption', async () => {
  // Encrypt with key derived from pw1; decrypt with independently derived key from pw1.
  const data = enc.encode('deterministic key test');
  const { nonce, ciphertext } = await T.encryptFile(data, pw1);
  // Derive a second key from the same password bytes independently.
  const key2 = await T.aesKey(pw1);
  const recovered = new Uint8Array(
    await crypto.subtle.decrypt({ name: 'AES-GCM', iv: nonce }, key2, ciphertext)
  );
  assert.deepEqual(recovered, data);
});

// ── formatBytes edge cases ────────────────────────────────────────────────

test('formatBytes at exact KB boundary', () => {
  assert.equal(T.formatBytes(1000), '1.0 KB');
  assert.equal(T.formatBytes(999), '999 B');
});

test('formatBytes at exact MB boundary', () => {
  assert.equal(T.formatBytes(1_000_000), '1.0 MB');
  assert.equal(T.formatBytes(999_999), '1000.0 KB');
});

test('formatBytes at exact GB boundary', () => {
  assert.equal(T.formatBytes(1_000_000_000), '1.0 GB');
});

// ── hexStream edge cases ──────────────────────────────────────────────────

test('hexStream with explicit length=0 returns empty string', () => {
  assert.equal(T.hexStream(0, 0), '');
});

test('hexStream with length=4 produces 4 hex chars, no spaces', () => {
  const out = T.hexStream(1, 4);
  assert.equal(out.length, 4);
  assert.match(out, /^[0-9a-f]{4}$/);
});

test('hexStream with length=8 produces 8 hex chars + 1 space', () => {
  // Groups of 4 separated by spaces; 8 chars = 2 groups, 1 space between.
  const out = T.hexStream(1, 8);
  assert.equal(out.length, 9); // 8 hex + 1 space
});

// ── genAccessPassword ─────────────────────────────────────────────────────

test('genAccessPassword contains only alphanumeric characters', () => {
  for (let i = 0; i < 50; i++) {
    assert.match(T.genAccessPassword(), /^[A-Za-z0-9]{32}$/);
  }
});

test('genAccessPassword uses the full character set (statistical)', () => {
  // Generate 200 passwords; across ~6400 chars we should see all 62 charset chars.
  const seen = new Set();
  for (let i = 0; i < 200; i++) {
    for (const ch of T.genAccessPassword()) seen.add(ch);
  }
  // Allow a small margin — in practice all 62 chars appear well within 200 runs.
  assert.ok(seen.size >= 50, `Only ${seen.size} distinct chars seen (expected ≥ 50 of 62)`);
});

// ── toggleReveal ──────────────────────────────────────────────────────────
// Uses plain objects — no DOM needed since toggleReveal operates on .type
// and .textContent properties.

test('toggleReveal: password→text reveals and labels "hide"', () => {
  const el = { type: 'password' };
  const btn = { textContent: 'reveal' };
  T.toggleReveal(el, btn);
  assert.equal(el.type, 'text');
  assert.equal(btn.textContent, 'hide');
});

test('toggleReveal: text→password conceals and labels "reveal"', () => {
  const el = { type: 'text' };
  const btn = { textContent: 'hide' };
  T.toggleReveal(el, btn);
  assert.equal(el.type, 'password');
  assert.equal(btn.textContent, 'reveal');
});

test('toggleReveal: double-toggle returns to original state', () => {
  const el = { type: 'password' };
  const btn = { textContent: 'reveal' };
  T.toggleReveal(el, btn);
  T.toggleReveal(el, btn);
  assert.equal(el.type, 'password');
  assert.equal(btn.textContent, 'reveal');
});

// ── isDecryptReady ────────────────────────────────────────────────────────

test('isDecryptReady: false when both empty', () => {
  assert.equal(T.isDecryptReady('', ''), false);
});

test('isDecryptReady: false when access empty, enc present', () => {
  assert.equal(T.isDecryptReady('', 'myenc'), false);
});

test('isDecryptReady: false when enc empty, access present', () => {
  assert.equal(T.isDecryptReady('myaccess', ''), false);
});

test('isDecryptReady: false when both whitespace-only', () => {
  assert.equal(T.isDecryptReady('   ', '\t'), false);
});

test('isDecryptReady: true when both non-empty', () => {
  assert.equal(T.isDecryptReady('access', 'enc'), true);
});

test('isDecryptReady: true when values contain surrounding whitespace', () => {
  assert.equal(T.isDecryptReady(' access ', ' enc '), true);
});

// ── fileExtension ─────────────────────────────────────────────────────────

test('fileExtension: standard extension', () => {
  assert.equal(T.fileExtension('report.pdf'), 'PDF');
});

test('fileExtension: extension capped at 3 chars', () => {
  assert.equal(T.fileExtension('archive.html'), 'HTM');
});

test('fileExtension: multi-dot filename uses last extension', () => {
  assert.equal(T.fileExtension('archive.tar.gz'), 'GZ');
});

test('fileExtension: no extension returns FILE', () => {
  assert.equal(T.fileExtension('Makefile'), 'FILE');
  assert.equal(T.fileExtension('README'), 'FILE');
});

test('fileExtension: empty string returns FILE', () => {
  assert.equal(T.fileExtension(''), 'FILE');
});

test('fileExtension: dotfile (leading dot) uses extension after dot', () => {
  assert.equal(T.fileExtension('.env'), 'ENV');
});

test('fileExtension: dotfile with extension uses last segment', () => {
  assert.equal(T.fileExtension('.eslintrc.json'), 'JSO');
});

// ── lifespanLabel ─────────────────────────────────────────────────────────

test('lifespanLabel: known values', () => {
  assert.equal(T.lifespanLabel(3600), '1 hour');
  assert.equal(T.lifespanLabel(86400), '24 hours');
  assert.equal(T.lifespanLabel(604800), '7 days');
  assert.equal(T.lifespanLabel(2592000), '30 days');
});

test('lifespanLabel: unknown value falls back to 7 days', () => {
  assert.equal(T.lifespanLabel(9999), '7 days');
  assert.equal(T.lifespanLabel(0), '7 days');
});

// ── downloadLimitLabel ────────────────────────────────────────────────────

test('downloadLimitLabel: null/undefined means unlimited', () => {
  assert.equal(T.downloadLimitLabel(null), 'unlimited downloads');
  assert.equal(T.downloadLimitLabel(0), 'unlimited downloads');
});

test('downloadLimitLabel: 1 is singular', () => {
  assert.equal(T.downloadLimitLabel(1), '1 download');
});

test('downloadLimitLabel: >1 is plural', () => {
  assert.equal(T.downloadLimitLabel(3), '3 downloads');
  assert.equal(T.downloadLimitLabel(10), '10 downloads');
});
