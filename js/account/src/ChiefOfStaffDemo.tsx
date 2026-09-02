import { Bot, ExternalLink, Plus, RefreshCw, Trash2 } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useAccountSession } from "./AccountSession";
import "./ChiefOfStaffDemo.css";

type Channel = Readonly<{
  availability: "ready" | "setup_required" | "not_enabled";
  contract: "first_party" | "vendor_official";
  detail: string;
  id: "slack" | "whatsapp" | "imessage";
}>;

type Readiness = Readonly<{
  accountMatch: boolean;
  channels: readonly Channel[];
  configured: boolean;
  installations: readonly Readonly<{
    botUserId: string | null;
    installedAt: number;
    teamId: string;
    teamName: string;
  }>[];
  installUrl: string | null;
  webhookUrl: string | null;
}>;

const labels = { slack: "Slack", whatsapp: "WhatsApp", imessage: "iMessage" } as const;
const docs = {
  slack: "https://chat-sdk.dev/adapters/slack",
  whatsapp: "https://chat-sdk.dev/adapters/whatsapp",
  imessage: "https://chat-sdk.dev/adapters/photon",
} as const;

export function ChiefOfStaffDemo() {
  const account = useAccountSession();
  const [readiness, setReadiness] = useState<Readiness | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [operation, setOperation] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const response = await fetch("/api/chief-of-staff/status", {
        credentials: "same-origin",
        headers: { accept: "application/json" },
      });
      if (!response.ok) throw new Error(response.status === 401
        ? "Your account session is not ready yet."
        : "The integration Worker is unavailable.");
      setReadiness(await response.json() as Readiness);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Couldn’t load integration readiness.");
      setReadiness(null);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (account.status === "checking") return;
    void refresh();
  }, [account.status, refresh]);

  const removeInstallation = useCallback(async (teamId: string) => {
    if (operation) return;
    setOperation(teamId);
    setError(null);
    try {
      const response = await fetch(
        `/api/chief-of-staff/slack/installations/${encodeURIComponent(teamId)}`,
        { method: "DELETE", credentials: "same-origin" },
      );
      if (!response.ok) throw new Error("Couldn’t remove the Slack app.");
      await refresh();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Couldn’t remove the Slack app.");
    } finally {
      setOperation(null);
    }
  }, [operation, refresh]);

  return (
    <article className="chief-demo page-grid">
      <header className="chief-hero">
        <p className="eyebrow">Demos · Chat SDK integration</p>
        <h1>Chief of Staff</h1>
        <p>
          Install a durable Nanocodex agent into Slack as its own AI teammate. It has a bot
          identity, receives mentions and DMs, and keeps every workspace and conversation on
          its own route.
        </p>
        <div className="chief-hero-status" aria-live="polite">
          <span className={`chief-status-dot${readiness?.configured ? " is-ready" : ""}`} />
          <span>{loading ? "Checking deployment" : readiness?.configured
            ? "Slack is ready"
            : "Slack setup required"}</span>
          <button type="button" onClick={() => void refresh()} disabled={loading}>
            <RefreshCw aria-hidden="true" /> Refresh
          </button>
        </div>
      </header>

      {error ? <section className="chief-error" role="alert">
        <span>{error}</span>
        <button type="button" onClick={() => void refresh()}>Try again</button>
      </section> : null}

      <section className="chief-channel-grid" aria-label="Channel readiness">
        {(readiness?.channels ?? fallbackChannels).map((channel) => (
          <article className={`chief-channel is-${channel.availability}`} key={channel.id}>
            <header>
              <div>
                <span>{labels[channel.id]}</span>
                <small>{channel.contract === "first_party" ? "First-party adapter" : "Vendor adapter"}</small>
              </div>
              <strong>{channel.availability === "ready" ? "Ready"
                : channel.availability === "setup_required" ? "Setup required" : "Not enabled"}</strong>
            </header>
            <p>{channel.detail}</p>
            <a href={docs[channel.id]} target="_blank" rel="noreferrer">
              Official Chat SDK contract <ExternalLink aria-hidden="true" />
            </a>
          </article>
        ))}
      </section>

      <section className="chief-setup" aria-labelledby="chief-setup-title">
        <header>
          <div>
            <p className="eyebrow">Slack AI bot</p>
            <h2 id="chief-setup-title">Add Chief of Staff to Slack</h2>
          </div>
          <Bot aria-hidden="true" />
        </header>
        <div className="chief-install-action">
          <div>
            <strong>One workspace approval</strong>
            <p>Slack shows the bot permissions, then installs the app and returns you here. No token or webhook setup.</p>
          </div>
          {readiness?.installUrl ? <a className="chief-add-slack" href={readiness.installUrl}>
            <Plus aria-hidden="true" /> Add to Slack
          </a> : <span className="chief-install-unavailable">Deployment setup required</span>}
        </div>
        {(readiness?.installations ?? []).map((installation) => (
          <div className="chief-installation" key={installation.teamId}>
            <div>
              <strong>{installation.teamName}</strong>
              <p>Bot installed{installation.botUserId ? ` as ${installation.botUserId}` : ""}</p>
            </div>
            <button
              type="button"
              disabled={operation !== null}
              onClick={() => void removeInstallation(installation.teamId)}
            >
              <Trash2 aria-hidden="true" />
              {operation === installation.teamId ? "Removing" : "Remove"}
            </button>
          </div>
        ))}
        <p className="chief-secret-note">
          This installs the AI bot. The separate Slack connector acts as your own Slack user and
          has its own authorization, tokens, and workspace grants.
        </p>
      </section>
    </article>
  );
}

const fallbackChannels: readonly Channel[] = [
  { id: "slack", availability: "setup_required", contract: "first_party", detail: "Readiness has not been confirmed by the integration Worker." },
  { id: "whatsapp", availability: "not_enabled", contract: "first_party", detail: "The SDK contract exists, but this deployment does not claim a configured Meta webhook." },
  { id: "imessage", availability: "not_enabled", contract: "vendor_official", detail: "Chat SDK catalogs vendor adapters; no iMessage provider is connected here." },
];
