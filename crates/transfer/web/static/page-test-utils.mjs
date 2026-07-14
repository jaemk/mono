// Shared helpers for page-controller tests.
//
// Each page module is a factory, create<Page>(win), bound to a window. These
// helpers build a jsdom window from the real page HTML and let tests inject a
// fake Transfer (the app.js helper surface) plus fakes for fetch / crypto /
// requestAnimationFrame, so the page logic is exercised in isolation without
// re-testing app.js or hitting the network.
import { JSDOM } from 'jsdom';
import { readFileSync } from 'node:fs';

const ROOT = new URL('../', import.meta.url); // crates/transfer/web/

// Build a window for `htmlFile` (relative to web/, e.g. 'download.html') that a
// page factory can bind to.
// opts.url       — page URL (drives location.search / URLSearchParams / origin)
// opts.Transfer  — fake injected as window.Transfer
// opts.fetch     — fake injected as window.fetch
// opts.crypto    — fake injected as window.crypto
// opts.location  — fake location (defaults to one derived from opts.url)
// Adds no-op requestAnimationFrame/cancelAnimationFrame unless provided.
//
// The returned value is a Proxy over the real jsdom window. The only override
// is `location`: jsdom marks window.location non-configurable and leaves
// navigation unimplemented, so a real `win.location.href = …` is neither
// redefinable nor observable. The Proxy swaps in a mutable fakeLocation so
// redirects are assertable, and writes to `win.location` land on it too.
// Everything else passes straight through to the real window; plain methods are
// bound to it so jsdom internals keep their expected `this` (constructors like
// Event/Blob/URL are left unbound so `new win.Event(...)` still works).
export function makeWindow(htmlFile, opts = {}) {
  const html = readFileSync(new URL(htmlFile, ROOT), 'utf8');
  // runScripts left at default: external <script src> tags are NOT executed,
  // so the page does not bootstrap itself — tests drive the factory directly.
  const dom = new JSDOM(html, { url: opts.url || 'https://transfer.test/' });
  const real = dom.window;
  if (opts.Transfer) real.Transfer = opts.Transfer;
  if (opts.fetch) real.fetch = opts.fetch;
  if (opts.crypto) Object.defineProperty(real, 'crypto', { value: opts.crypto, configurable: true });
  real.requestAnimationFrame = opts.requestAnimationFrame || (() => 0);
  real.cancelAnimationFrame = opts.cancelAnimationFrame || (() => {});

  const loc = opts.location || fakeLocation({
    href: real.location.href,
    search: real.location.search,
    origin: real.location.origin,
  });

  const isConstructor = (prop) => typeof prop === 'string' && /^[A-Z]/.test(prop);

  return new Proxy(real, {
    get(target, prop, receiver) {
      if (prop === 'location') return loc;
      const value = Reflect.get(target, prop, target);
      // Bind plain (lowercase) methods so jsdom keeps `this === real window`.
      if (typeof value === 'function' && !isConstructor(prop)) return value.bind(target);
      return value;
    },
    set(target, prop, value) {
      if (prop === 'location') { loc.href = value; return true; }
      return Reflect.set(target, prop, value);
    },
  });
}

// A mutable, observable stand-in for window.location. `href` assignments are
// recorded so tests can assert redirects; `search` drives URLSearchParams.
export function fakeLocation({ href = 'https://transfer.test/', search = '', origin = 'https://transfer.test' } = {}) {
  return { href, search, origin, assign(u) { this.href = u; } };
}

// A spy that records its calls and returns a fixed value (or runs an impl).
export function spy(implOrValue) {
  const calls = [];
  const isFn = typeof implOrValue === 'function';
  const fn = (...args) => {
    calls.push(args);
    return isFn ? implOrValue(...args) : implOrValue;
  };
  fn.calls = calls;
  return fn;
}

// Minimal, deterministic stand-ins for the app.js helpers a page may call.
// Tests override individual members as needed.
export function fakeTransfer(overrides = {}) {
  const enc = new TextEncoder();
  return {
    BASE: '/transfer/api',
    isDecryptReady: (a, b) => Boolean(String(a).trim()) && Boolean(String(b).trim()),
    toHex: (bytes) => Array.from(bytes).map(b => b.toString(16).padStart(2, '0')).join(''),
    fromHex: (hex) => {
      const arr = new Uint8Array(hex.length / 2);
      for (let i = 0; i < hex.length; i += 2) arr[i / 2] = parseInt(hex.slice(i, i + 2), 16);
      return arr;
    },
    formatBytes: (n) => n + ' B',
    fileExtension: (name) => {
      const parts = String(name).split('.');
      return parts.length < 2 ? 'FILE' : (parts[parts.length - 1] || '').toUpperCase().slice(0, 3) || 'FILE';
    },
    hexStream: () => 'deadbeef',
    lifespanLabel: (s) => String(s),
    downloadLimitLabel: (n) => (n ? `${n} downloads` : 'unlimited downloads'),
    genAccessPassword: () => 'A'.repeat(32),
    escapeHtml: (s) => String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;'),
    sleep: async () => {},
    encode: (s) => enc.encode(s),
    ...overrides,
  };
}
