import { useEffect, useRef, useState } from "react";
import { signInWithGoogle, type GoogleAccount } from "./googleSignIn.js";

export function GoogleSignInButton({ authOrigin, disabled, onSignIn, onError, onBusyChange, intent = "sign_in" }: {
  authOrigin?: string;
  disabled?: boolean;
  intent?: "sign_in" | "link";
  onSignIn(account: GoogleAccount): void;
  onError(message: string | undefined): void;
  onBusyChange?(busy: boolean): void;
}) {
  const active = useRef<AbortController | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => () => { active.current?.abort(); active.current = null; }, []);
  const start = async () => {
    if (active.current || disabled) return;
    const controller = new AbortController();
    active.current = controller;
    setBusy(true); onBusyChange?.(true); onError(undefined);
    try {
      const account = await signInWithGoogle({ authOrigin, signal: controller.signal, intent });
      if (active.current === controller && !controller.signal.aborted) onSignIn(account);
    } catch (error) {
      if (active.current === controller && !controller.signal.aborted) {
        onError(error instanceof DOMException && error.name === "AbortError"
          ? undefined : error instanceof Error ? error.message : "Couldn’t finish Google sign-in. Please try again.");
      }
    } finally {
      if (active.current === controller) {
        active.current = null; setBusy(false); onBusyChange?.(false);
      }
    }
  };
  return <div className="google-sign-in">
    <button className="google-sign-in-button" type="button" disabled={disabled || busy} aria-busy={busy} onClick={() => void start()}>
      <span className="google-sign-in-logo" aria-hidden="true" />
      <span>{busy ? "Waiting for Google…" : intent === "link" ? "Link Google account" : "Sign in with Google"}</span>
    </button>
    {busy ? <button className="google-sign-in-cancel" type="button" onClick={() => active.current?.abort()}>Cancel Google sign-in</button> : null}
  </div>;
}
