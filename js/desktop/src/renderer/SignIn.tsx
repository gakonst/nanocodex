import { useEffect, useRef, useState } from "react";
import { ArrowLeft, ArrowUpRight, LoaderCircle, Terminal } from "lucide-react";
import type { DesktopState } from "../shared/types";

const bridge = window.nanocodex;
type Challenge = Awaited<ReturnType<typeof bridge.startSignIn>>;
const message = (cause: unknown) =>
  (cause instanceof Error ? cause.message : String(cause)).replace(
    /^Error invoking remote method '[^']+': Error: /,
    ""
  );

export function SignIn({
  baseUrl,
  onSignedIn,
  onCancel,
}: {
  baseUrl: string;
  onSignedIn(state: DesktopState): void;
  onCancel?(): void;
}) {
  const [phone, setPhone] = useState("");
  const [code, setCode] = useState("");
  const [challenge, setChallenge] = useState<Challenge>();
  const [origin, setOrigin] = useState(baseUrl);
  const [key, setKey] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [now, setNow] = useState(Date.now());
  const mounted = useRef(true);
  const completing = useRef(false);
  const codeInput = useRef<HTMLInputElement>(null);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      if (!completing.current) void bridge.cancelSignIn().catch(() => {});
    };
  }, []);
  useEffect(() => {
    if (!challenge) return;
    setNow(Date.now());
    codeInput.current?.focus();
    const timer = setInterval(() => setNow(Date.now()), 1_000);
    return () => clearInterval(timer);
  }, [challenge]);
  const resendSeconds = challenge
    ? Math.max(0, Math.ceil((challenge.resendAt - now) / 1_000))
    : 0;
  const expired = !!challenge && now >= challenge.expiresAt;
  async function run(work: () => Promise<void>) {
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      await work();
    } catch (cause) {
      if (mounted.current) setError(message(cause));
    } finally {
      if (mounted.current) setBusy(false);
    }
  }
  function start() {
    return run(async () => {
      const next = await bridge.startSignIn({ phone, baseUrl: origin });
      if (mounted.current) {
        setChallenge(next);
        setPhone(next.phone);
        setCode("");
      }
    });
  }
  function back() {
    return run(async () => {
      await bridge.cancelSignIn();
      if (mounted.current) {
        setChallenge(undefined);
        setCode("");
      }
    });
  }
  return (
    <div className="sign-in" aria-busy={busy}>
      <div className="sign-in-mark">
        <Terminal size={30} strokeWidth={1.5} />
      </div>
      <h1>{challenge ? "Check your messages" : "Welcome to Nanocodex"}</h1>
      <p className="sign-in-intro">
        {challenge ? (
          <>
            Enter the 6-digit code sent to <strong>{challenge.phone}</strong>.
          </>
        ) : (
          "Sign in with your phone number. Your threads, models, and Hands stay with your account."
        )}
      </p>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          if (!challenge) void start();
          else
            void run(async () => {
              completing.current = true;
              try {
                const state = await bridge.verifySignIn({ code });
                onSignedIn(state);
              } catch (cause) {
                if (!mounted.current)
                  void bridge.cancelSignIn().catch(() => {});
                throw cause;
              } finally {
                completing.current = false;
              }
            });
        }}
      >
        {challenge ? (
          <label>
            Verification code
            <input
              ref={codeInput}
              aria-label="Verification code"
              aria-describedby="sign-in-code-help"
              className="verification-code"
              type="text"
              inputMode="numeric"
              autoComplete="one-time-code"
              pattern="[0-9]{6}"
              maxLength={6}
              placeholder="000000"
              value={code}
              required
              disabled={busy || expired}
              onChange={(event) =>
                setCode(event.target.value.replace(/\D/g, "").slice(0, 6))
              }
            />
          </label>
        ) : (
          <label>
            Phone number
            <input
              aria-label="Phone number"
              aria-describedby="sign-in-phone-help"
              type="tel"
              autoComplete="tel"
              autoFocus
              placeholder="+1 415 555 0123"
              value={phone}
              required
              disabled={busy}
              onChange={(event) => setPhone(event.target.value)}
            />
          </label>
        )}
        <p
          className="sign-in-help"
          id={challenge ? "sign-in-code-help" : "sign-in-phone-help"}
        >
          {challenge
            ? expired
              ? "This code has expired. Request a new one below."
              : "You’ll stay signed in on this computer."
            : "Include your country code. We’ll text you a verification code."}
        </p>
        {error && (
          <div className="sign-in-error" role="alert">
            {error}
          </div>
        )}
        <button
          className="primary-button sign-in-submit"
          disabled={
            busy || (challenge ? code.length !== 6 || expired : !phone.trim())
          }
        >
          {busy && <LoaderCircle size={15} className="spin" />}
          {busy
            ? challenge
              ? "Signing in…"
              : "Sending code…"
            : challenge
            ? "Sign in"
            : "Continue"}
        </button>
      </form>
      {challenge && (
        <div className="sign-in-links">
          <button
            className="text-button"
            disabled={busy}
            onClick={() => void back()}
          >
            <ArrowLeft size={13} />
            Change number
          </button>
          <button
            className="text-button"
            disabled={busy || resendSeconds > 0}
            onClick={() => void start()}
          >
            {resendSeconds > 0
              ? `Resend code in ${resendSeconds}s`
              : "Resend code"}
          </button>
        </div>
      )}
      {!challenge && (
        <details className="sign-in-advanced">
          <summary>Advanced</summary>
          <label>
            Managed service
            <input
              aria-label="Managed service"
              type="url"
              value={origin}
              disabled={busy}
              onChange={(event) => setOrigin(event.target.value)}
            />
          </label>
          <form
            onSubmit={(event) => {
              event.preventDefault();
              void run(async () => {
                const state = await bridge.connect({
                  baseUrl: origin,
                  apiKey: key.trim(),
                  remember: true,
                });
                if (mounted.current) {
                  setKey("");
                  onSignedIn(state);
                }
              });
            }}
          >
            <label>
              Account API key
              <input
                aria-label="Nanocodex API key"
                type="password"
                autoComplete="off"
                placeholder="Paste an existing API key"
                value={key}
                required
                disabled={busy}
                onChange={(event) => setKey(event.target.value)}
              />
            </label>
            <button className="secondary-button" disabled={busy || !key.trim()}>
              Connect with API key
            </button>
          </form>
          <button
            className="text-button"
            disabled={busy}
            onClick={() => void run(async () => bridge.openAccount())}
          >
            Open account settings
            <ArrowUpRight size={13} />
          </button>
        </details>
      )}
      {onCancel && (
        <button
          className="text-button sign-in-cancel"
          disabled={busy}
          onClick={() =>
            void run(async () => {
              await bridge.cancelSignIn();
              if (mounted.current) onCancel();
            })
          }
        >
          Cancel
        </button>
      )}
    </div>
  );
}
