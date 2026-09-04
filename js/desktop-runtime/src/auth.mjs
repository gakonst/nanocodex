import { hostname } from "node:os";
import { DEFAULT_ORIGIN, managedOrigin } from "./runtime.mjs";

const KEY = /^ncx_live_([A-Za-z0-9_-]{12})_[A-Za-z0-9_-]{43}$/;
const CHALLENGE = /^[A-Za-z0-9_-]{43}$/;
const SESSION = /^nanocodex_account=(s_[A-Za-z0-9_-]{43})(?:;|$)/;

export class SmsSignInError extends Error {
  constructor(code, message, retryAt) {
    super(message);
    this.name = "SmsSignInError";
    this.code = code;
    if (retryAt !== undefined) this.retryAt = retryAt;
  }
}

function failure(code, status, body = {}, headers) {
  const seconds = Number(headers?.get("retry-after") ?? body.retry_after);
  if (status === 429 || code === "rate_limited") {
    const wait = Number.isFinite(seconds) && seconds > 0 ? Math.min(Math.ceil(seconds), 3600) : 60;
    return new SmsSignInError("rate_limited", `Please wait ${wait} seconds before trying again.`, Date.now() + wait * 1000);
  }
  const messages = {
    invalid_phone: "Enter your phone number with its country code, such as +1 415 555 0123.",
    invalid_otp: "Enter the six-digit code from your text message.",
    invalid_or_expired_otp: "That code is incorrect or has expired. Try again or request a new code.",
    sms_otp_unavailable: "Phone sign-in is temporarily unavailable. Please try again shortly.",
    sms_delivery_failed: "We could not send your code. Check your number and try again.",
    sms_verification_failed: "We could not check your code. Please try again.",
    sms_identity_unavailable: "We could not finish signing you in. Please try again.",
    wallet_unavailable: "Your account is still being prepared. Please try the code again shortly.",
    unauthorized: "Your sign-in expired. Request a new code to continue.",
    forbidden: "This account cannot authorize the app. Contact your account administrator.",
  };
  const known = Object.hasOwn(messages, code);
  return new SmsSignInError(known || code === "not_found" ? code : "sign_in_failed", known ? messages[code]
    : "We could not finish signing you in. Please try again.");
}

/** Owns one private SMS session until the caller has saved its key in the OS
 * credential store. Never publish this instance or verify() results to a web UI. */
export class SmsSignIn {
  #baseUrl;
  #attempt;
  #generation = 0;
  #queue = Promise.resolve();

  constructor({ baseUrl = DEFAULT_ORIGIN } = {}) {
    this.#baseUrl = managedOrigin(baseUrl);
  }

  #serial(work) {
    const pending = this.#queue.then(work);
    this.#queue = pending.catch(() => {});
    return pending;
  }

  #current(generation) {
    if (generation !== this.#generation) throw new SmsSignInError("cancelled", "Sign-in was cancelled.");
  }

  async #request(path, { method = "POST", body, cookie } = {}) {
    let response;
    try {
      response = await fetch(new URL(path, this.#baseUrl), {
        method, redirect: "error", signal: AbortSignal.timeout(20_000),
        headers: {
          "content-type": "application/json", origin: this.#baseUrl,
          ...(cookie ? { cookie } : {}),
        },
        ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      });
      const encoded = await response.text();
      if (encoded.length > 16_384) throw new Error("invalid response");
      let value;
      try { value = encoded ? JSON.parse(encoded) : {}; } catch { value = {}; }
      if (!value || typeof value !== "object" || Array.isArray(value)) value = {};
      if (!response.ok) throw failure(typeof value.error === "string" ? value.error : undefined, response.status, value, response.headers);
      return { body: value, headers: response.headers };
    } catch (error) {
      if (error instanceof SmsSignInError) throw error;
      throw new SmsSignInError("network", "We could not reach Nanocodex. Check your connection and try again.");
    }
  }

  start({ phone } = {}) {
    const normalized = typeof phone === "string" ? phone.trim().replace(/[\s().-]/g, "") : "";
    if (!/^\+[1-9]\d{7,14}$/.test(normalized)) return Promise.reject(failure("invalid_phone", 400));
    const generation = ++this.#generation;
    return this.#serial(async () => {
      this.#current(generation);
      if (this.#attempt?.phone === normalized && Date.now() < this.#attempt.resendAt) {
        throw failure("rate_limited", 429, { retry_after: Math.ceil((this.#attempt.resendAt - Date.now()) / 1000) });
      }
      await this.#discard(true);
      this.#current(generation);
      const { body } = await this.#request("/v1/auth/sms/start", { body: { phone: normalized } });
      if (!CHALLENGE.test(body.challenge_id) || !Number.isInteger(body.expires_in) || body.expires_in < 1 || body.expires_in > 3600
        || !Number.isInteger(body.resend_after) || body.resend_after < 0 || body.resend_after > 3600) {
        throw failure("invalid_response", 502);
      }
      const now = Date.now();
      this.#attempt = { phone: normalized, challengeId: body.challenge_id, expiresAt: now + body.expires_in * 1000, resendAt: now + body.resend_after * 1000 };
      this.#current(generation);
      return { phone: normalized, expiresAt: this.#attempt.expiresAt, resendAt: this.#attempt.resendAt };
    });
  }

  verify({ code } = {}) {
    const generation = this.#generation;
    return this.#serial(async () => {
      this.#current(generation);
      const attempt = this.#attempt;
      if (!attempt) throw new SmsSignInError("not_started", "Request a text message code first.");
      if (!attempt.cookie) {
        if (Date.now() >= attempt.expiresAt) throw new SmsSignInError("expired", "Your code has expired. Request a new code.");
        const normalized = typeof code === "string" ? code.replace(/\s/g, "") : "";
        if (!/^\d{6}$/.test(normalized)) throw failure("invalid_otp", 400);
        const { headers } = await this.#request("/v1/auth/sms/verify", {
          body: { phone: attempt.phone, challenge_id: attempt.challengeId, code: normalized },
        });
        const cookie = headers.getSetCookie().map(value => SESSION.exec(value)).find(Boolean);
        if (!cookie) throw failure("invalid_response", 502);
        attempt.cookie = `nanocodex_account=${cookie[1]}`;
        delete attempt.challengeId;
        this.#current(generation);
      }
      if (!attempt.key) {
        const { body } = await this.#request("/v1/api-keys", {
          cookie: attempt.cookie,
          body: { label: `Nanocodex on ${hostname().replace(/\.local$/i, "")}`.slice(0, 120) },
        });
        const matched = typeof body.api_key === "string" ? KEY.exec(body.api_key) : undefined;
        if (!matched) throw failure("invalid_response", 502);
        // Keep the exact minted credential before observing cancellation, so
        // cancel() can revoke it even when the HTTP response raced the user.
        attempt.key = { id: matched[1], apiKey: body.api_key };
        this.#current(generation);
      }
      return { baseUrl: this.#baseUrl, apiKey: attempt.key.apiKey };
    });
  }

  complete() {
    return this.#serial(() => this.#discard(false));
  }

  cancel() {
    ++this.#generation;
    return this.#serial(() => this.#discard(true));
  }

  async #discard(revoke) {
    const attempt = this.#attempt;
    if (!attempt) return;
    if (revoke && attempt.key) {
      try { await this.#request(`/v1/api-keys/${attempt.key.id}`, { method: "DELETE", cookie: attempt.cookie }); }
      catch (error) { if (error.code !== "not_found") throw error; }
    }
    this.#attempt = undefined;
    if (attempt.cookie) {
      // Completion follows successful OS persistence. A logout failure must
      // never revoke that saved key or retain the temporary browser session.
      await this.#request("/v1/auth/logout", { cookie: attempt.cookie }).catch(() => {});
    }
  }
}
