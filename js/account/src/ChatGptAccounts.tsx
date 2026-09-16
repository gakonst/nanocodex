import { ConnectionLogo } from "nanocodex-connect-ui/ConnectionLogo";
import { useEffect, useRef } from "react";
import type { CredentialStatus } from "./modelCredentials";
import "./ChatGptAccounts.css";

export function ChatGptAccounts({ status, disabled, onAdd, onDisconnect }: Readonly<{
  status: CredentialStatus["chatgpt"];
  disabled: boolean;
  onAdd(): void;
  onDisconnect(): void;
}>) {
  const card = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (window.location.hash === "#chatgpt-accounts") card.current?.scrollIntoView({ block: "center" });
  }, []);
  return (
    <div className="wizard-connector-card chatgpt-accounts" id="chatgpt-accounts" role="listitem" ref={card}>
      <button className={`connection-card${status.connected ? " is-connected" : ""}`}
        type="button" disabled={disabled || Boolean(status.login)} onClick={onAdd}>
        <ConnectionLogo id="chatgpt" />
        <span className="connection-card-copy">
          <strong>ChatGPT</strong>
          <span>{status.accounts.length ? `${status.accounts.length} account${status.accounts.length === 1 ? "" : "s"} added`
            : "Use your ChatGPT subscription for model access"}</span>
        </span>
        <span className="connection-card-action">{status.login ? "Signing in…" : status.accounts.length ? "Add account" : "Connect"}</span>
      </button>
      {status.accounts.length > 0 ? (
        <div className="chatgpt-account-details">
          <ul aria-label="ChatGPT accounts">
            {status.accounts.map((account) => (
              <li key={account.accountId}>
                <code>{account.accountId}</code>
                <span>{!account.connected ? "Sign in again"
                  : account.limitedUntil && account.limitedUntil > Date.now()
                    ? `Limit reached · resets ${new Date(account.limitedUntil).toLocaleString()}`
                    : account.active ? "Default account" : "Available"}</span>
              </li>
            ))}
          </ul>
          <p>Sessions without an account override switch automatically when an account reaches its limit.</p>
          <button type="button" disabled={disabled || Boolean(status.login)} onClick={onDisconnect}>
            Disconnect all ChatGPT accounts
          </button>
        </div>
      ) : null}
    </div>
  );
}
