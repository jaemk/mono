// Client-side AES-GCM-256 encryption using the Web Crypto API.
// No external dependencies.

const BASE = '/transfer/api';

// ── Crypto helpers ────────────────────────────────────────────────────────

async function sha256(data) {
  return new Uint8Array(await crypto.subtle.digest('SHA-256', data));
}

async function aesKey(passwordBytes) {
  const raw = await sha256(passwordBytes);
  return crypto.subtle.importKey('raw', raw, { name: 'AES-GCM' }, false, ['encrypt', 'decrypt']);
}

function randomBytes(n) {
  return crypto.getRandomValues(new Uint8Array(n));
}

async function encryptFile(fileBytes, passwordBytes) {
  const nonce = randomBytes(12);
  const key = await aesKey(passwordBytes);
  const ciphertext = new Uint8Array(
    await crypto.subtle.encrypt({ name: 'AES-GCM', iv: nonce }, key, fileBytes)
  );
  return { nonce, ciphertext };
}

async function decryptFile(ciphertextBytes, nonce, passwordBytes) {
  const key = await aesKey(passwordBytes);
  return new Uint8Array(
    await crypto.subtle.decrypt({ name: 'AES-GCM', iv: nonce }, key, ciphertextBytes)
  );
}

async function encryptFilename(name, passwordBytes) {
  const enc = new TextEncoder();
  const { nonce, ciphertext } = await encryptFile(enc.encode(name), passwordBytes);
  const combined = new Uint8Array(nonce.length + ciphertext.length);
  combined.set(nonce);
  combined.set(ciphertext, nonce.length);
  return combined;
}

async function decryptFilename(combined, passwordBytes) {
  const nonce = combined.slice(0, 12);
  const ciphertext = combined.slice(12);
  const plain = await decryptFile(ciphertext, nonce, passwordBytes);
  return new TextDecoder().decode(plain);
}

// ── Encoding helpers ──────────────────────────────────────────────────────

function toHex(bytes) {
  return Array.from(bytes).map(b => b.toString(16).padStart(2, '0')).join('');
}

function fromHex(hex) {
  const arr = new Uint8Array(hex.length / 2);
  for (let i = 0; i < hex.length; i += 2) arr[i / 2] = parseInt(hex.slice(i, i + 2), 16);
  return arr;
}

// ── API helpers ───────────────────────────────────────────────────────────

async function apiGet(path) {
  const res = await fetch(BASE + path);
  const json = await res.json();
  if (!res.ok) throw new Error(json.error || res.statusText);
  return json;
}

async function apiJson(path, body) {
  const res = await fetch(BASE + path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  const json = await res.json();
  if (!res.ok) throw new Error(json.error || res.statusText);
  return json;
}

async function apiUploadBytes(key, bytes) {
  const res = await fetch(`${BASE}/upload?key=${key}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/octet-stream' },
    body: bytes,
  });
  const json = await res.json();
  if (!res.ok) throw new Error(json.error || res.statusText);
  return json;
}

// ── Utilities ─────────────────────────────────────────────────────────────

function formatBytes(n) {
  if (n >= 1e9) return (n / 1e9).toFixed(1) + ' GB';
  if (n >= 1e6) return (n / 1e6).toFixed(1) + ' MB';
  if (n >= 1e3) return (n / 1e3).toFixed(1) + ' KB';
  return n + ' B';
}

function sleep(ms) {
  return new Promise(r => setTimeout(r, ms));
}

// Deterministic pseudo-random hex stream for animation visuals.
function hexStream(seed, len = 28) {
  let s = seed;
  let out = '';
  const chars = '0123456789abcdef';
  for (let i = 0; i < len; i++) {
    s = (s * 1103515245 + 12345) & 0x7fffffff;
    out += chars[(s >> 8) & 15];
    if (i % 4 === 3 && i < len - 1) out += ' ';
  }
  return out;
}

// ── UI helpers ────────────────────────────────────────────────────────────

// Toggle a password input between hidden (type="password") and visible
// (type="text"), updating a companion button's label.
function toggleReveal(el, btn) {
  const revealed = el.type === 'text';
  el.type = revealed ? 'password' : 'text';
  btn.textContent = revealed ? 'reveal' : 'hide';
}

// Returns true only when both password values are non-empty after trimming.
function isDecryptReady(accessVal, encVal) {
  return Boolean(accessVal.trim()) && Boolean(encVal.trim());
}

// Derive a short uppercase extension label (≤3 chars) from a filename.
function fileExtension(name) {
  const parts = name.split('.');
  if (parts.length < 2) return 'FILE';
  const ext = (parts[parts.length - 1] || '').toUpperCase().slice(0, 3);
  return ext || 'FILE';
}

// Human-readable label for common lifespan values (in seconds).
function lifespanLabel(secs) {
  return { 3600: '1 hour', 86400: '24 hours', 604800: '7 days', 2592000: '30 days' }[secs] || '7 days';
}

// Human-readable download-limit label.
function downloadLimitLabel(limit) {
  if (!limit) return 'unlimited downloads';
  return `${limit} download${limit !== 1 ? 's' : ''}`;
}

// ── Password helpers ──────────────────────────────────────────────────────

function genAccessPassword() {
  const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
  const bytes = randomBytes(32);
  return Array.from(bytes).map(b => chars[b % chars.length]).join('');
}

// ── Exports ───────────────────────────────────────────────────────────────

const Transfer = {
  sha256, aesKey, randomBytes,
  encryptFile, decryptFile,
  encryptFilename, decryptFilename,
  toHex, fromHex,
  apiGet, apiJson, apiUploadBytes,
  formatBytes, sleep, hexStream,
  genAccessPassword,
  toggleReveal, isDecryptReady,
  fileExtension, lifespanLabel, downloadLimitLabel,
  BASE,
};
// Browser: expose as window.Transfer; Node (tests): export as CommonJS module.
if (typeof window !== 'undefined') window.Transfer = Transfer;
if (typeof module !== 'undefined') module.exports = Transfer;
