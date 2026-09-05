import { useId, useState, type FormEvent, type ReactNode } from "react";

import { ConnectionLogo } from "./ConnectionLogo.js";
import type { McpConnection, McpConnectionStatus } from "./connectTypes.js";

export const chatGptCredentialImportAction = "Will import from Codex";
export const chatGptCredentialImportHelper = "After you approve, the Nanocodex CLI will send the ChatGPT sign-in already stored by Codex directly to Nanocodex. This page cannot access it.";
export const chatGptCredentialImportApproved = "Approved. Return to the terminal while Nanocodex finishes the import.";

export function AccountConnectionSurface({
  children,
  confirmationCode,
  confirmationLabel = "Confirm this matches your terminal",
  description,
  footer,
  title,
}: Readonly<{
  children: ReactNode;
  confirmationCode?: string | undefined;
  confirmationLabel?: string | undefined;
  description: ReactNode;
  footer?: ReactNode;
  title: string;
}>) {
  return (
    <div className="wizard-page wizard-review-page">
      <header className="wizard-intro">
        <div className="wizard-app">
          <h1>{title}</h1>
          <p>{description}</p>
        </div>
        {confirmationCode ? (
          <div className="wizard-terminal-code" role="status">
            <span>{confirmationLabel}</span>
            <strong>{confirmationCode.slice(0, 4)}-{confirmationCode.slice(4)}</strong>
          </div>
        ) : null}
      </header>
      <div className="wizard-sections">{children}</div>
      {footer}
    </div>
  );
}

export function AccountConnectionSection({
  children,
  eyebrow,
  meta,
  title,
  titleId,
}: Readonly<{
  children: ReactNode;
  eyebrow: string;
  meta?: ReactNode;
  title: string;
  titleId: string;
}>) {
  return (
    <section className="wizard-section" aria-labelledby={titleId}>
      <header className="wizard-section-title">
        <div><span>{eyebrow}</span><h2 id={titleId}>{title}</h2></div>
        {meta === undefined ? null : <small>{meta}</small>}
      </header>
      {children}
    </section>
  );
}

export function AccountConnectionGrid({ children }: Readonly<{ children: ReactNode }>) {
  return <div className="wizard-connectors" role="list">{children}</div>;
}

export function AccountConnectionCard({
  action,
  connected = false,
  detail,
  disabled,
  logo,
  onClick,
  title,
}: Readonly<{
  action: string;
  connected?: boolean | undefined;
  detail: string;
  disabled: boolean;
  logo: ReactNode;
  onClick(): void;
  title: string;
}>) {
  return (
    <div className="wizard-connector-card" role="listitem">
      <button
        className={`connection-card${connected ? " is-connected" : ""}`}
        disabled={disabled}
        onClick={onClick}
        type="button"
      >
        {logo}
        <span className="connection-card-copy">
          <strong>{title}</strong>
          <span>{detail}</span>
        </span>
        <span className="connection-card-action">{action}</span>
      </button>
    </div>
  );
}

export function McpConnectionAddCard({
  disabled = false,
  error,
  listItem = true,
  onSubmit,
}: Readonly<{
  disabled?: boolean | undefined;
  error?: string | undefined;
  listItem?: boolean | undefined;
  onSubmit(target: string): Promise<boolean>;
}>) {
  const [target, setTarget] = useState("");
  const id = useId();
  const targetId = `mcp-connection-target-${id}`;
  const errorId = `mcp-connection-target-error-${id}`;

  const submit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (!target.trim() || disabled) return;
    if (await onSubmit(target)) setTarget("");
  };

  return (
    <div
      className="connection-card connector-row mcp-connector-row mcp-connection-add"
      role={listItem ? "listitem" : undefined}
    >
      <ConnectionLogo id="mcp" />
      <form className="connection-card-copy mcp-connection-add-form" onSubmit={(event) => void submit(event)}>
        <label htmlFor={targetId}><strong>Add MCP connection</strong></label>
        <span>Linear shorthand or a public HTTPS endpoint</span>
        <div className="mcp-connection-actions">
          <input
            aria-describedby={error ? errorId : undefined}
            aria-invalid={error ? true : undefined}
            autoComplete="url"
            disabled={disabled}
            id={targetId}
            inputMode="url"
            onChange={(event) => setTarget(event.target.value)}
            placeholder="mcp.linear.app or https://…"
            required
            size={20}
            style={{ minHeight: 44, fontSize: 16 }}
            type="text"
            value={target}
          />
          <button disabled={disabled} type="submit">Add MCP</button>
        </div>
        {error ? <small id={errorId} role="alert">{error}</small> : null}
      </form>
    </div>
  );
}

export function McpConnectionCard({
  action,
  actionDisabled = false,
  connection,
  error,
  listItem = true,
  onAction,
  presentation = "connect",
}: Readonly<{
  action?: string | undefined;
  actionDisabled?: boolean | undefined;
  connection: McpConnection;
  error?: string | undefined;
  listItem?: boolean | undefined;
  onAction?: (() => void) | undefined;
  presentation?: "account" | "connect" | undefined;
}>) {
  const connected = connection.status === "connected";
  const status = mcpConnectionStatusLabel(connection.status);

  const account = presentation === "account";
  return (
    <div
      className={account
        ? `connection-card connector-row mcp-connector-row${connected ? " is-connected" : ""}`
        : `mcp-connection-card${connected ? " is-connected" : ""}`}
      role={listItem ? "listitem" : undefined}
    >
      {account ? <ConnectionLogo id="mcp" /> : (
        <span className="mcp-connection-logo" aria-hidden="true">M</span>
      )}
      <span className={account ? "connection-card-copy" : "mcp-connection-copy"}>
        <strong>{connection.name}</strong>
        <span>{status}</span>
        {error ? <small className="mcp-connection-error" role="alert">{error}</small> : null}
      </span>
      <span className="mcp-connection-actions">
        {action && onAction ? (
          <button disabled={actionDisabled} onClick={onAction} type="button">{action}</button>
        ) : (
          <span className={account ? "connection-card-action" : "mcp-connection-state"}>
            {action ?? status}
          </span>
        )}
      </span>
    </div>
  );
}

export function mcpConnectionStatusLabel(status: McpConnectionStatus): string {
  if (status === "connected") return "Connected";
  if (status === "authorization_required") return "Authorization required";
  if (status === "reauthorization_required") return "Reconnect required";
  if (status === "disabled") return "Disabled";
  return "Revoked";
}

export function DeferredChatGptImportCard() {
  return (
    <AccountConnectionCard
      action={chatGptCredentialImportAction}
      detail={chatGptCredentialImportHelper}
      disabled
      logo={<ConnectionLogo id="chatgpt" />}
      onClick={() => undefined}
      title="ChatGPT"
    />
  );
}

export function DeferredChatGptImportStatus({ approved }: Readonly<{ approved: boolean }>) {
  return <>{approved ? chatGptCredentialImportApproved : chatGptCredentialImportHelper}</>;
}
