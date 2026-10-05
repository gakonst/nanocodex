import { useState } from "react";
import { GoogleSignInButton } from "nanocodex-connect-ui/GoogleSignInButton";
import { useAccountSession } from "./AccountSession";

export function AccountSignInMethods() {
  const { account, operation } = useAccountSession();
  const [error, setError] = useState<string>();
  const [linked, setLinked] = useState(false);
  if (!account?.persistent) return null;
  return <section className="connect-onboarding account-sign-in-methods" aria-label="Sign-in methods">
    <h2>Sign-in methods</h2>
    <p>Link Google to sign in to this same account on any device.</p>
    <GoogleSignInButton intent="link" disabled={operation !== null}
      onError={(message) => { setError(message); setLinked(false); }}
      onSignIn={(next) => {
        if (next.id !== account.id) { setError("Your account changed. Refresh before trying again."); return; }
        setLinked(true);
      }} />
    {error ? <p role="alert">{error}</p> : null}
    {linked ? <p role="status">Google is linked. You can now use it to sign in to this account.</p> : null}
  </section>;
}
