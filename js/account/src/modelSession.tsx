import { useCallback, useEffect, useRef, useState } from "react";
import { useAccountSession } from "./AccountSession";
import { GenerationRequestOwner } from "./agentTerminalLifecycle";
import type { AgentStatus } from "./agentTerminalTypes";
import { clientFailureMessage } from "./clientFailure";
import { deploymentHealth } from "./deploymentHealth";
import { invalidateModelHealthForAccountTransition } from "./modelHealthAccount";

export type CredentialSource = "brokered" | "sponsored" | null;
export type ModelSessionStatus =
  | { state: "signed_out" }
  | { state: "ready"; freePromptsRemaining: number | null; ready: boolean; voiceEnabled: boolean }
  | { state: "error"; error: string };

export type SessionPresentation = {
  agentError?: string;
  agentStatus: AgentStatus;
  authStatus: ModelSessionStatus | undefined;
  capabilityError?: string;
  runtime?: "browser" | "managed";
  source: CredentialSource | undefined;
};

const ACCOUNT_TRANSITION_RETRY_DELAYS_MS = [300, 900, 1_800] as const;

export function inactiveTerminalMessage({
  agentError,
  agentStatus,
  authStatus,
  capabilityError,
  runtime = "browser",
  source,
}: SessionPresentation): string {
  if (capabilityError) return capabilityError;
  if (agentStatus === "starting") return "";
  if (agentStatus === "error" && source) return agentStartFailure(agentError);
  if (source === undefined || authStatus === undefined) return "";
  const agent = runtime === "managed" ? "managed agent" : "browser agent";
  if (authStatus.state === "signed_out") return `Verify your phone by SMS to start the ${agent}.`;
  if (authStatus.state === "error") return "Could not check your model connection. Use Retry above.";
  if (runtime === "managed" && source === "sponsored") {
    return "The included model is limited to the ephemeral homepage demo. Connect ChatGPT or an OpenAI API key to use durable agents.";
  }
  if (runtime === "browser" && source === "sponsored" && authStatus.freePromptsRemaining === 0) {
    return "Your three free prompts are used. Connect ChatGPT or an OpenAI API key to continue.";
  }
  if (!authStatus.ready) {
    return `Connect ChatGPT or an OpenAI API key from the account menu to start the ${agent}.`;
  }
  return "";
}

export function agentStartFailure(error?: string): string {
  if (error && /WebAssembly|CompileError|wasm/i.test(error)) {
    return "The browser agent could not initialize WebAssembly. Reload once, then update Safari or use another current browser if it continues.";
  }
  if (error && /Origin Private File System|OPFS|Web Locks/i.test(error)) {
    return "The browser agent could not open its private workspace. Allow website storage, close duplicate tabs, and retry.";
  }
  return error ? `Agent start failed: ${error}` : "Could not start the agent. Use Retry agent above.";
}

export function AgentSessionBar({
  agentError,
  agentStatus,
  capabilityError,
  source,
  onAuthStatusChange,
  onRetryAgent,
  onSourceChange,
}: {
  agentError: string | undefined;
  agentStatus: AgentStatus;
  capabilityError: string | undefined;
  source: CredentialSource | undefined;
  onAuthStatusChange(status: ModelSessionStatus): void;
  onRetryAgent(): void;
  onSourceChange(source: CredentialSource): void;
}) {
  const { busy, retrySession, status } = useModelSession({
    onStatusChange: onAuthStatusChange,
    onSourceChange,
  });
  const ready = agentStatus === "ready";
  const hasCredential = source === "brokered" || source === "sponsored";
  const label = sessionLabel({ agentStatus, authStatus: status, capabilityError, source });
  const compactReady = ready
    && hasCredential
    && status?.state === "ready"
    && status.ready
    && !agentError
    && !capabilityError;

  return (
    <div className={`agent-session-shell${compactReady ? " is-compact-ready" : ""}`}>
      <div className="agent-session-bar">
        <span className="agent-session-status" aria-live="polite">
          <i className={ready ? "is-ready" : ""} aria-hidden="true" />
          {label}
        </span>
        <div className="agent-session-actions">
          {status?.state === "error" || (status?.state === "ready" && !status.ready) ? (
            <button type="button" onClick={retrySession} disabled={busy}>retry connection</button>
          ) : null}
          {agentStatus === "error" && hasCredential ? (
            <button type="button" onClick={onRetryAgent}>retry agent</button>
          ) : null}
        </div>
      </div>
      {capabilityError ? <p className="agent-byok-error" role="alert">{capabilityError}</p> : null}
      {status?.state === "error" ? (
        <p className="agent-byok-error" role="alert">{status.error}</p>
      ) : null}
      {status?.state === "ready" && !status.ready ? (
        <p className="agent-session-note" role="status">
          Connect ChatGPT or an OpenAI API key from the account menu.
        </p>
      ) : null}
      {status?.state === "signed_out" ? (
        <p className="agent-session-note" role="status">Verify your phone by SMS from the account menu.</p>
      ) : null}
      {agentError ? (
        <p className="agent-byok-error" role="alert">
          {agentStatus === "error" ? agentStartFailure(agentError) : agentError}
        </p>
      ) : null}
    </div>
  );
}

export function useModelSession({
  onStatusChange,
  onSourceChange,
}: {
  onStatusChange(status: ModelSessionStatus): void;
  onSourceChange(source: CredentialSource): void;
}) {
  const accountSession = useAccountSession();
  const account = accountSession.account;
  const [status, setStatus] = useState<ModelSessionStatus>();
  const [busy, setBusy] = useState(false);
  const generation = useRef(0);
  const observedAccountId = useRef<string | undefined>(undefined);
  const requests = useRef(new GenerationRequestOwner<
    Awaited<ReturnType<typeof deploymentHealth.read>> | undefined
  >());
  const publish = useCallback((next: ModelSessionStatus, source: CredentialSource) => {
    setStatus(next);
    onStatusChange(next);
    onSourceChange(source);
  }, [onSourceChange, onStatusChange]);
  const readStatus = useCallback((fresh: boolean) => {
    const current = ++generation.current;
    return requests.current.run(current, async () => {
      if (accountSession.status !== "ready") return;
      if (!account) {
        publish({ state: "signed_out" }, null);
        return;
      }
      try {
        const health = await (fresh
          ? deploymentHealth.refresh()
          : deploymentHealth.read());
        if (generation.current !== current) return;
        publish({
          state: "ready",
          freePromptsRemaining: health.freePromptsRemaining,
          ready: health.agentConfigured,
          voiceEnabled: health.voiceEnabled,
        }, health.credentialSource);
        return health;
      } catch (cause) {
        if (generation.current !== current) return;
        publish({
          state: "error",
          error: clientFailureMessage(cause, "Could not check the model connection. Try again."),
        }, null);
      }
    });
  }, [account, accountSession.status, publish]);

  useEffect(() => {
    if (accountSession.status !== "ready") {
      generation.current++;
      return;
    }
    const previousAccountId = observedAccountId.current;
    const accountId = account?.id;
    observedAccountId.current = accountId;
    const accountChanged = invalidateModelHealthForAccountTransition(
      previousAccountId,
      accountId,
      deploymentHealth,
    );
    if (!account) {
      generation.current++;
      publish({ state: "signed_out" }, null);
      return;
    }
    let cancelled = false;
    let retry: number | undefined;
    const retryWhileStarting = (
      health: Awaited<ReturnType<typeof deploymentHealth.read>> | undefined,
      attempt = 0,
    ) => {
      if (cancelled || health?.agentConfigured !== false
        || attempt >= ACCOUNT_TRANSITION_RETRY_DELAYS_MS.length) return;
      retry = window.setTimeout(() => {
        void readStatus(true).then((next) => retryWhileStarting(next, attempt + 1));
      }, ACCOUNT_TRANSITION_RETRY_DELAYS_MS[attempt]);
    };
    void readStatus(accountChanged).then((health) => retryWhileStarting(health));
    return () => {
      cancelled = true;
      if (retry !== undefined) window.clearTimeout(retry);
    };
  }, [account, accountSession.status, publish, readStatus]);
  useEffect(() => {
    let inactive = false;
    const becameInactive = () => { inactive = true; };
    const refreshAfterInactivity = () => {
      if (!inactive || document.visibilityState !== "visible") return;
      inactive = false;
      void readStatus(true);
    };
    const visibilityChanged = () => {
      if (document.visibilityState === "hidden") becameInactive();
      else refreshAfterInactivity();
    };
    const pageShown = (event: PageTransitionEvent) => {
      if (!event.persisted) return;
      inactive = false;
      void readStatus(true);
    };
    const credentialChanged = () => { void readStatus(true); };
    window.addEventListener("blur", becameInactive);
    window.addEventListener("focus", refreshAfterInactivity);
    window.addEventListener("pageshow", pageShown);
    window.addEventListener("nanocodex:model-credential-changed", credentialChanged);
    document.addEventListener("visibilitychange", visibilityChanged);
    return () => {
      window.removeEventListener("blur", becameInactive);
      window.removeEventListener("focus", refreshAfterInactivity);
      window.removeEventListener("pageshow", pageShown);
      window.removeEventListener("nanocodex:model-credential-changed", credentialChanged);
      document.removeEventListener("visibilitychange", visibilityChanged);
    };
  }, [readStatus]);

  const retrySession = async () => {
    setBusy(true);
    try { await readStatus(true); } finally { setBusy(false); }
  };
  return { busy, retrySession, status };
}

function sessionLabel({
  agentStatus,
  authStatus,
  capabilityError,
  source,
}: SessionPresentation): string {
  if (capabilityError) return "browser unsupported";
  if (agentStatus === "starting" && source) return "agent starting";
  if (agentStatus === "ready") return "agent ready";
  if (agentStatus === "error" && source) return "agent unavailable";
  if (authStatus?.state === "signed_out") return "account required";
  if (authStatus?.state === "error") return "connection check failed";
  if (authStatus?.state === "ready" && !authStatus.ready) return "model connection required";
  return "";
}
