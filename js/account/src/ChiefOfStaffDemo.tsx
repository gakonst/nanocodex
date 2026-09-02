import { Check, Copy, ExternalLink, RefreshCw, ShieldCheck } from "lucide-react";
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
  const [copied, setCopied] = useState(false);

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

  const copyWebhook = useCallback(async () => {
    if (!readiness?.webhookUrl) return;
    await navigator.clipboard.writeText(readiness.webhookUrl);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1_500);
  }, [readiness?.webhookUrl]);

  return (
    <article className="chief-demo page-grid">
      <header className="chief-hero">
        <p className="eyebrow">Demos · Chat SDK integration</p>
        <h1>Chief of Staff</h1>
        <p>
          Bring one account-owned, durable Nanocodex agent into supported messaging channels.
          The integration Worker verifies provider traffic and keeps every account, workspace,
          channel, and conversation on its own route.
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
            <p className="eyebrow">Slack deployment</p>
            <h2 id="chief-setup-title">Wire the signed ingress</h2>
          </div>
          <ShieldCheck aria-hidden="true" />
        </header>
        <ol>
          <li>
            <span>01</span>
            <div><strong>Bind the account</strong><p>Set the Worker-only <code>NANOCODEX_API_KEY</code>. Its account must match this page’s signed-in account.</p></div>
          </li>
          <li>
            <span>02</span>
            <div><strong>Configure Slack</strong><p>Set <code>SLACK_BOT_TOKEN</code>, <code>SLACK_SIGNING_SECRET</code>, <code>SLACK_BOT_USER_ID</code>, and <code>SLACK_TEAM_ID</code> as Worker secrets.</p></div>
          </li>
          <li>
            <span>03</span>
            <div><strong>Register the webhook</strong><p>Use this URL for Slack Events API requests. Subscribe to app mentions and message events for each enabled channel type.</p></div>
            {readiness?.webhookUrl ? <button type="button" onClick={() => void copyWebhook()}>
              {copied ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
              {copied ? "Copied" : "Copy URL"}
            </button> : null}
          </li>
        </ol>
        {readiness?.webhookUrl ? <code className="chief-webhook">{readiness.webhookUrl}</code> : null}
        <p className="chief-secret-note">Credential values stay inside Worker bindings and are never returned to this application.</p>
      </section>
    </article>
  );
}

const fallbackChannels: readonly Channel[] = [
  { id: "slack", availability: "setup_required", contract: "first_party", detail: "Readiness has not been confirmed by the integration Worker." },
  { id: "whatsapp", availability: "not_enabled", contract: "first_party", detail: "The SDK contract exists, but this deployment does not claim a configured Meta webhook." },
  { id: "imessage", availability: "not_enabled", contract: "vendor_official", detail: "Chat SDK catalogs vendor adapters; no iMessage provider is connected here." },
];
