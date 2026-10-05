export type GoogleAccount = Readonly<{ id: string; address: `0x${string}`; persistent: true }>;

const TIMEOUT_MS = 20_000;
const ATTEMPT_ID = /^[A-Za-z0-9_-]{43}$/;

/** Opens synchronously in the click gesture. Proofs live only in this operation's memory. */
export async function signInWithGoogle({ authOrigin = "", signal, intent = "sign_in" }: {
  authOrigin?: string;
  signal: AbortSignal;
  intent?: "sign_in" | "link";
}): Promise<GoogleAccount> {
  const origin = new URL(authOrigin || window.location.origin).origin;
  const popup = window.open("about:blank", "_blank", "popup,width=500,height=680");
  if (!popup) throw new Error("Allow pop-ups for Nanocodex, then try Google sign-in again.");
  // Provider pages never get a reference to the account page.
  popup.opener = null;
  popup.document.title = "Sign in with Google";
  popup.document.body.textContent = "Opening Google sign-in…";
  const verifier = base64url(crypto.getRandomValues(new Uint8Array(32)));
  let attemptId: string | undefined;
  let completed = false;
  try {
    const challenge = base64url(new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(verifier))));
    signal.throwIfAborted();
    const start = await request("start", { mode: "browser", code_challenge: challenge, ...(intent === "link" ? { intent } : {}) }, signal);
    if (!isRecord(start) || typeof start.attempt_id !== "string" || !ATTEMPT_ID.test(start.attempt_id)
      || typeof start.authorization_url !== "string" || typeof start.expires_in !== "number"
      || !Number.isFinite(start.expires_in) || start.expires_in <= 0 || start.expires_in > 600) {
      throw new Error("The account service returned an invalid Google sign-in.");
    }
    attemptId = start.attempt_id;
    const authorization = new URL(start.authorization_url);
    if (authorization.origin !== origin || authorization.pathname !== "/v1/auth/google/authorize"
      || authorization.username || authorization.password || authorization.hash) {
      throw new Error("The account service returned an invalid Google sign-in address.");
    }
    signal.throwIfAborted();
    popup.location.replace(authorization.href);
    const deadline = Date.now() + start.expires_in * 1000;
    const proof = { attempt_id: attemptId, code_verifier: verifier };
    while (Date.now() < deadline) {
      signal.throwIfAborted();
      const result = await request("status", proof, signal);
      if (!isRecord(result)) throw new Error("The account service returned an invalid Google sign-in status.");
      if (result.status === "ready") {
        signal.throwIfAborted();
        // Complete once only. A lost response must never silently retry a login.
        const session = await request("complete", proof, signal);
        if (!isRecord(session) || !isRecord(session.user) || typeof session.user.id !== "string"
          || typeof session.user.address !== "string" || !/^0x[0-9a-f]{40}$/.test(session.user.address)
          || session.user.persistent !== true) throw new Error("The account service returned an invalid session.");
        completed = true;
        return session.user as GoogleAccount;
      }
      if (result.status === "cancelled") throw new DOMException("Google sign-in was cancelled.", "AbortError");
      if (result.status === "failed" && result.error === "google_access_denied") throw new DOMException("Google sign-in was cancelled.", "AbortError");
      if (result.status === "failed") throw new Error(googleFailure(result.error));
      if (result.status !== "pending") throw new Error("The account service returned an invalid Google sign-in status.");
      // Google may sever its window reference using COOP. Do not mistake that
      // for user cancellation; the visible Cancel control always remains usable.
      await delay(1000, signal);
    }
    throw new Error("Google sign-in expired. Please try again.");
  } finally {
    try { popup.close(); } catch { /* Provider isolated its window. */ }
    if (attemptId && !completed) {
      // Cancellation cannot create credentials. Keep it independent of a disposed UI signal.
      void request("cancel", { attempt_id: attemptId, code_verifier: verifier }, AbortSignal.timeout(TIMEOUT_MS)).catch(() => {});
    }
  }

  async function request(path: string, body: Record<string, unknown>, outerSignal: AbortSignal): Promise<unknown> {
    const response = await fetch(`${origin}/v1/auth/google/${path}`, {
      method: "POST", credentials: "include", cache: "no-store", redirect: "error",
      headers: { "content-type": "application/json", accept: "application/json" },
      body: JSON.stringify(body), signal: AbortSignal.any([outerSignal, AbortSignal.timeout(TIMEOUT_MS)]),
    });
    const value: unknown = response.status === 204 ? undefined : await response.json().catch(() => undefined);
    if (!response.ok) throw new Error(googleFailure(isRecord(value) ? value.error : undefined));
    return value;
  }
}

function googleFailure(code: unknown): string {
  if (code === "google_sign_in_unavailable") return "Google sign-in is temporarily unavailable. You can sign in with your phone.";
  if (code === "google_identity_already_linked") return "This Google account is already linked to another Nanocodex account.";
  if (code === "google_link_requires_account" || code === "google_link_requires_same_account") return "Your account changed or signed out. Sign in again before linking Google.";
  if (code === "rate_limited") return "Too many sign-in attempts. Wait a minute and try again.";
  if (code === "google_sign_in_expired" || code === "invalid_google_attempt" || code === "invalid_or_expired_google_attempt") return "Google sign-in expired. Please try again.";
  if (code === "wallet_unavailable" || code === "google_account_unavailable") return "Your account is still being prepared. Please try signing in again.";
  return "Couldn’t finish Google sign-in. Please try again.";
}
function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
function base64url(bytes: Uint8Array): string {
  return btoa(String.fromCharCode(...bytes)).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
}
function delay(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    const abort = () => { clearTimeout(timer); reject(signal.reason); };
    const timer = setTimeout(() => { signal.removeEventListener("abort", abort); resolve(); }, ms);
    if (signal.aborted) abort();
    else signal.addEventListener("abort", abort, { once: true });
  });
}
