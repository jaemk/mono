// Page controller for register.html — the account-creation + email-verify flow.
//
// Structured as a factory, createRegisterPage(win), that binds to a given
// window (its document, location, Transfer helpers, timers, …). All state lives
// in the returned closure, so unit tests can build an independent instance
// against a jsdom window without leaking global state between cases.
//
// Browser: a single instance is created from the real `window`, exposed as
// window.TransferRegister, and init() runs the load flow + wires DOM events.
// Node (tests): `createRegisterPage` is exported via CommonJS.

(function () {
  'use strict';

  function createRegisterPage(win) {
    const doc = win.document;
    const T = win.Transfer;

    const $ = (id) => doc.getElementById(id);

    // ── State ───────────────────────────────────────────────────────────────
    let pendingId   = null;
    let resendTimer = null;
    let resendSecs  = 0;

    // ── Register ──────────────────────────────────────────────────────────────
    async function doRegister() {
      const email    = $('reg-email').value.trim();
      const password = $('reg-password').value;
      if (!email)              return showError('Enter your email address.');
      if (password.length < 8) return showError('Password must be at least 8 characters.');
      const btn = $('reg-btn');
      btn.disabled = true; btn.textContent = 'Creating account…';
      hideError();
      try {
        const r = await T.apiJson('/auth/register', { email, password });
        pendingId = r.pending_id;
        $('verify-email-display').textContent = email;
        $('stage-form').style.display = 'none';
        $('stage-verify').style.display = '';
        $('page-title').textContent = 'Verify your email';
        $('page-sub').textContent = 'Enter the code we sent you to complete registration.';
        startResendCountdown(60);
      } catch (e) {
        showError(e.message || 'Registration failed');
        btn.disabled = false; btn.textContent = 'Create account';
      }
    }

    // ── Verify ────────────────────────────────────────────────────────────────
    async function doVerify() {
      const code = $('verify-code').value.trim();
      if (code.length !== 6) return showError('Enter the 6-digit code from your email.');
      const btn = $('verify-btn');
      btn.disabled = true; btn.textContent = 'Verifying…';
      hideError();
      try {
        await T.apiJson('/auth/verify-code', { pending_id: pendingId, code });
        win.location.href = '/transfer/my';
      } catch (e) {
        showError(e.message || 'Verification failed');
        btn.disabled = false; btn.textContent = 'Verify';
      }
    }

    // ── Resend ────────────────────────────────────────────────────────────────
    async function doResend() {
      if (!pendingId) return;
      const btn = $('resend-btn');
      btn.disabled = true;
      hideError();
      try {
        await T.apiJson('/auth/resend-code', { pending_id: pendingId });
        startResendCountdown(60);
      } catch (e) {
        showError(e.message || 'Failed to resend');
        btn.disabled = false;
      }
    }

    // ── Resend countdown ──────────────────────────────────────────────────────
    function startResendCountdown(secs) {
      resendSecs = secs;
      const btn = $('resend-btn');
      const countdown = $('resend-countdown');
      btn.disabled = true;
      win.clearInterval(resendTimer);
      resendTimer = win.setInterval(() => {
        resendSecs--;
        if (resendSecs <= 0) {
          win.clearInterval(resendTimer);
          countdown.textContent = '';
          btn.disabled = false;
        } else {
          countdown.textContent = 'Resend in ' + resendSecs + 's';
        }
      }, 1000);
      countdown.textContent = 'Resend in ' + resendSecs + 's';
    }

    // ── Error ─────────────────────────────────────────────────────────────────
    function showError(msg) {
      const e = $('error-box');
      e.textContent = msg;
      e.style.display = '';
    }
    function hideError() {
      $('error-box').style.display = 'none';
    }

    // ── Init: run the load flow + wire DOM events ─────────────────────────────
    async function init() {
      const me = await T.checkAuth();
      if (me) { win.location.href = '/transfer/my'; return; }
      T.updateTopbar(null);

      try {
        const d = await T.apiGet('/upload/defaults');
        if (!d.email_enabled) {
          $('stage-form').innerHTML =
            '<div class="tx-meta" style="text-align:center">Email registration is not enabled on this server.</div>';
        }
      } catch {}

      // The register form may have been replaced above (email disabled), so
      // those controls can be absent; the verify stage is always present.
      const regBtn = $('reg-btn');
      if (regBtn) regBtn.addEventListener('click', doRegister);
      const regPw = $('reg-password');
      if (regPw) regPw.addEventListener('keydown', (e) => { if (e.key === 'Enter') doRegister(); });
      $('verify-btn').addEventListener('click', doVerify);
      $('verify-code').addEventListener('keydown', (e) => { if (e.key === 'Enter') doVerify(); });
      $('resend-btn').addEventListener('click', doResend);
    }

    return {
      init,
      doRegister, doVerify, doResend,
      startResendCountdown, showError, hideError,
      // read-only accessor for assertions
      get pendingId() { return pendingId; },
    };
  }

  // ── Browser bootstrap ───────────────────────────────────────────────────────
  if (typeof window !== 'undefined') {
    const page = createRegisterPage(window);
    window.TransferRegister = page;
    page.init();
  }

  if (typeof module !== 'undefined' && module.exports) {
    module.exports = { createRegisterPage };
  }
})();
