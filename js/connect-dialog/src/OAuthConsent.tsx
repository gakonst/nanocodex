import { useEffect, useRef, useState } from "react";
import "./OAuthConsent.css";
import { AccountChooser } from "nanocodex-connect-ui/AccountChooser";
import { ConnectionLogo, type ConnectionLogoId } from "nanocodex-connect-ui/ConnectionLogo";
import {
  BrowserAccountReauthenticationRequiredError,
  logoutBrowserAccountSession,
  readBrowserAccountSession,
  type BrowserAccountSession,
} from "nanocodex-connect-ui/browserAccountSession";
import {
  isLocalDevelopmentOrigin,
  productionConnectApiOrigin,
} from "nanocodex-connect-ui/connectPolicy.mjs";

const routingHeaders = { "x-nanocodex-connect-client": "onboarding" };
const opaqueId = /^[A-Za-z0-9_-]{43}$/;
type ConsentRequest = Readonly<{
  client_id: string;
  client_name: string;
  app_id: string;
  app_origin: string;
  redirect_uri: string;
  resource: string;
  resources: readonly string[];
  scope: string;
  base_resources: readonly string[];
  scope_resources: Readonly<Record<string, readonly string[]>>;
}>;

/** A top-level, server-bound OAuth request; never accepts app claims from a parent. */
export function OAuthConsent() {
  const [request, setRequest] = useState<ConsentRequest>();
  const [selectedScopes, setSelectedScopes] = useState<readonly string[]>([]);
  const [account, setAccount] = useState<BrowserAccountSession | null>();
  const [connectors, setConnectors] = useState<Readonly<Record<string, unknown>>>();
  const [connectorFailure, setConnectorFailure] = useState<string>();
  const [failure, setFailure] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [finished, setFinished] = useState(false);
  const operation = useRef(false);
  const heading = useRef<HTMLHeadingElement>(null);
  useEffect(() => { if (account?.address) heading.current?.focus(); }, [account?.address]);
  const requestIds = new URLSearchParams(window.location.search).getAll("oauth_request");
  const requestId = requestIds.length === 1 && opaqueId.test(requestIds[0]!) ? requestIds[0]! : undefined;
  const apiOrigin = oauthIssuer(new URL(window.location.href));
  const requestUrl = `${apiOrigin}/oauth/requests/${requestId}`;

  useEffect(() => {
    const abort = new AbortController();
    if (!requestId || !apiOrigin || window.parent !== window) {
      setFailure("This authorization link is invalid. Start a new connection from your MCP client.");
      return;
    }
    void (async () => {
      try {
        const response = await fetch(requestUrl, {
          cache: "no-store", credentials: "omit", headers: routingHeaders, signal: abort.signal,
        });
        const body: unknown = await response.json();
        if (!response.ok || !isConsentRequest(body)) {
          throw new Error("This authorization request is invalid, expired, or already used. Start a new connection from your MCP client.");
        }
        if (abort.signal.aborted) return;
        setRequest(body);
        setSelectedScopes(body.scope.split(" ").includes("agent:run") ? ["agent:run"] : []);
        try {
          const session = await readBrowserAccountSession();
          if (!abort.signal.aborted) setAccount(session?.persistent && session.address ? session : null);
        } catch (error) {
          if (error instanceof BrowserAccountReauthenticationRequiredError) {
            if (!abort.signal.aborted) setAccount(null);
          } else throw error;
        }
      } catch (error) {
        if (!abort.signal.aborted) setFailure(errorMessage(error));
      }
    })();
    return () => abort.abort();
  }, [requestId, requestUrl, apiOrigin]);

  useEffect(() => {
    const abort = new AbortController();
    setConnectors(undefined);
    setConnectorFailure(undefined);
    if (!account?.address) return;
    void (async () => {
      try {
        const response = await fetch("/v1/connectors", {
          credentials: "same-origin", cache: "no-store", signal: abort.signal,
        });
        const body: unknown = await response.json();
        if (!response.ok || !isRecord(body) || !isRecord(body.connectors)) {
          throw new Error("Connected accounts could not be checked. Reload this page to try again.");
        }
        if (abort.signal.aborted) return;
        const statuses = body.connectors;
        setConnectors(statuses);
        setSelectedScopes(current => current.filter(scope => scopeAvailable(scope, statuses)));
      } catch (error) {
        if (!abort.signal.aborted) setConnectorFailure(errorMessage(error));
      }
    })();
    return () => abort.abort();
  }, [account?.address]);

  const selectableScopes = selectedScopes.filter(scope => scopeAvailable(scope, connectors));

  const requestedScopes = request?.scope.split(" ") ?? [];
  const serviceScopes = requestedScopes.filter(scope => requiredConnector(scope));
  const connectedScopes = serviceScopes.filter(scope => scopeAvailable(scope, connectors));
  const unavailableScopes = serviceScopes.filter(scope => !scopeAvailable(scope, connectors));
  const capabilityGroups = [...new Set(requestedScopes.filter(scope => !requiredConnector(scope)).map(scope => scope.split(":")[0]!))];
  const allServicesSelected = connectedScopes.length > 0 && connectedScopes.every(scope => selectableScopes.includes(scope));

  function scopeChoice(scope: string, compact = false) {
    const available = scopeAvailable(scope, connectors);
    return <label key={scope} className={compact ? "oauth-scope-chip" : "oauth-service-choice"} title={scopeLabel(scope)}>
      <input type="checkbox" checked={available && selectedScopes.includes(scope)}
        disabled={busy || finished || !available} aria-label={scopeLabel(scope)}
        onChange={event => setSelectedScopes(current => event.target.checked
          ? [...new Set([...current, scope])] : current.filter(value => value !== scope))} />
      <>{!compact ? <ConnectionLogo id={serviceLogo(scope)} /> : null}<span>{compact ? scopeAction(scope) : serviceLabel(scope).replace(/^Google /, "")}</span></>
    </label>;
  }

  async function settle(approve: boolean) {
    if (!request || operation.current || finished || (approve && (!account?.address || !selectableScopes.length))) return;
    operation.current = true;
    setBusy(true);
    setFailure(undefined);
    const scopes = request.scope.split(" ").filter(scope => selectableScopes.includes(scope));
    const resources = [...new Set([...request.base_resources, ...scopes.flatMap(scope => request.scope_resources[scope]!)])];
    let submitted = false;
    try {
      let code: string | undefined;
      if (approve) {
        const authorization = await fetch("/v1/connect/hosted-authorization/authorize", {
          method: "POST", credentials: "same-origin",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            account_address: account!.address,
            app_id: request.app_id,
            app_origin: request.app_origin,
            resources,
          }),
        });
        const body: unknown = await authorization.json();
        if (authorization.status === 401 || authorization.status === 403) {
          setAccount(null);
          throw new Error("Your account session changed or expired. Sign in again, then review and approve access.");
        }
        if (!authorization.ok || !isRecord(body) || typeof body.code !== "string" || !opaqueId.test(body.code)) {
          throw new Error(responseDescription(body) ?? "Your account could not authorize this request. Try again.");
        }
        code = body.code;
      }
      // A failed transport may still have consumed the request. Never retry an
      // approval or denial automatically (or expose a second submit button).
      submitted = true;
      const response = await fetch(`${requestUrl}/${approve ? "approve" : "deny"}`, {
        method: "POST", credentials: "omit",
        headers: { ...routingHeaders, "content-type": "application/json" },
        body: JSON.stringify(approve ? {
          account_address: account!.address, code, resources, scope: scopes.join(" "),
        } : {}),
      });
      const result: unknown = await response.json();
      if (!response.ok || !isRecord(result) || typeof result.redirect_uri !== "string") {
        throw new Error(isRecord(result) && typeof result.error_description === "string" && result.error_description.length <= 1_000
          ? result.error_description : "The authorization could not be completed. Start a new connection from your MCP client.");
      }
      const target = callbackUrl(result.redirect_uri, request.redirect_uri);
      setFinished(true);
      window.location.assign(target.href);
    } catch (error) {
      setFailure(errorMessage(error));
      if (submitted) setFinished(true);
    } finally {
      operation.current = false;
      setBusy(false);
    }
  }

  async function changeAccount() {
    if (operation.current) return;
    operation.current = true;
    setBusy(true);
    setFailure(undefined);
    try {
      await logoutBrowserAccountSession();
      setAccount(null);
    } catch (error) {
      setFailure(errorMessage(error));
    } finally {
      operation.current = false;
      setBusy(false);
    }
  }

  return <section className="connect-onboarding dialog-shell oauth-consent" data-request="oauth-consent" data-phase={account ? "review" : "signin"}>
    <header className="dialog-header"><span className="wordmark">Nanocodex <span>Connect</span></span></header>
    <div className="dialog-content">
      {request && !finished ? <>
        {account === undefined && !failure ? <p className="oauth-loading" role="status">Checking your account…</p> : null}
        {account === null ? <>
          <AccountChooser appName="Nanocodex" disabled={busy}
            requestContext={<p className="oauth-login-context">Continue to <strong>{request.client_name}</strong></p>}
            onCancel={() => void settle(false)}
            onChooseAccount={selected => {
              if (selected.address) {
                setAccount({ id: "authenticated", address: selected.address, persistent: true });
                setFailure(undefined);
              } else setFailure("Your account did not provide an address. Sign in again.");
            }} />
        </> : null}
        {account ? <div className="oauth-layout">
          <section className="request-title" aria-labelledby="oauth-heading">
            <div className="oauth-identity" aria-hidden="true">
              <span className="oauth-app-mark">{request.client_name.slice(0, 1).toUpperCase()}</span>
              <svg className="oauth-link" viewBox="0 0 24 24"><path d="M5 12h14m-5-5 5 5-5 5" /></svg>
              <span className="oauth-brand-mark"><svg viewBox="0 0 40 40"><path d="M11 29V11l18 18V11" /><circle cx="34" cy="29" r="1.5" /></svg></span>
            </div>
            <h1 id="oauth-heading" ref={heading} tabIndex={-1}>Connect {request.client_name}</h1>
            <p className="oauth-callback" title={request.redirect_uri}>{new URL(request.redirect_uri).host}</p>
            <button type="button" className="oauth-account" aria-label="Switch account" title={account.address} disabled={busy} onClick={() => void changeAccount()}>
              <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="8" r="3" /><path d="M5 21v-2a7 7 0 0 1 14 0v2" /></svg>
              <span>{account.address && account.address.length > 20 ? `${account.address.slice(0, 6)}…${account.address.slice(-4)}` : account.address}</span>
              <svg className="oauth-chevron" viewBox="0 0 24 24" aria-hidden="true"><path d="m8 10 4 4 4-4" /></svg>
            </button>
          </section>
          <section className="oauth-permissions" aria-label="Requested access">
            <div className="oauth-section-heading"><h2>Allow access to</h2><span className="oauth-selection-count" role="status">{selectableScopes.length} selected</span></div>
            {capabilityGroups.length > 0 ? <div className="oauth-capabilities">{capabilityGroups.map(group => <div className="oauth-capability-row" key={group}>
              <div className="oauth-capability-name"><CapabilityIcon name={group} /><h3>{capabilityLabel(group)}</h3></div>
              <div className="oauth-scope-options" role="group" aria-label={capabilityLabel(group)}>
                {requestedScopes.filter(scope => !requiredConnector(scope) && scope.split(":")[0] === group).map(scope => scopeChoice(scope, true))}
              </div>
            </div>)}</div> : null}
            {serviceScopes.length > 0 ? <section className="oauth-services" aria-labelledby="oauth-services-heading">
              <div className="oauth-section-heading">
                <h3 id="oauth-services-heading">Connected apps</h3>
                {connectedScopes.length > 0 ? <button className="oauth-text-action" type="button" disabled={busy}
                  aria-label={allServicesSelected ? "Clear services" : "Select all services"}
                  onClick={() => setSelectedScopes(current => allServicesSelected
                    ? current.filter(scope => !connectedScopes.includes(scope))
                    : [...new Set([...current, ...connectedScopes])])}>
                  {allServicesSelected ? "Clear" : "Select all"}
                </button> : null}
              </div>
              {!connectors && !connectorFailure ? <p className="oauth-hint" role="status">Checking connected apps…</p> : null}
              {connectorFailure ? <p className="dialog-error" role="alert">{connectorFailure}</p> : null}
              {connectedScopes.length > 0 ? <div className="oauth-service-grid">{connectedScopes.map(scope => scopeChoice(scope))}</div> : null}
              {connectors && connectedScopes.length === 0 ? <p className="oauth-hint">No connected apps available.</p> : null}
              {connectors && unavailableScopes.length > 0 ? <details className="oauth-disclosure">
                <summary>Not connected <span>{unavailableScopes.length}</span></summary>
                <div className="oauth-service-grid">{unavailableScopes.map(scope => scopeChoice(scope))}</div>
              </details> : null}
            </section> : null}
          </section>
        </div> : null}
      </> : !request && !failure ? <p className="oauth-loading" role="status">Loading connection…</p> : null}
      {failure ? <p className="dialog-error" role="alert">{failure}</p> : null}
      {finished && !failure ? <p className="oauth-loading" role="status">Returning to your app…</p> : null}
    </div>
    {request && account && !finished ? <div className="dialog-actions"><div className="oauth-action-buttons">
      <button type="button" disabled={busy} onClick={() => void settle(false)}>Deny</button>
      <button type="button" disabled={busy || selectableScopes.length === 0} aria-busy={busy} onClick={() => void settle(true)}>
        {busy ? "Connecting…" : "Allow access"}
      </button>
    </div></div> : null}
  </section>;
}

function CapabilityIcon({ name }: Readonly<{ name: string }>) {
  const paths: Record<string, string> = {
    agent: "m12 3 2.5 6.5L21 12l-6.5 2.5L12 21l-2.5-6.5L3 12l6.5-2.5Z",
    agents: "M7 5h10v14H7zM3 8v8m18-8v8M10 9h4m-4 6h4",
    history: "M4 11a8 8 0 1 1 2 6M4 5v6h6m2-4v5l3 2",
    memory: "M6 3h12v18l-6-4-6 4Z",
    data: "M5 4h14v16H5zM5 9h14M10 9v11",
    tools: "m4 20 7-7m3-9a5 5 0 0 0-4 7 5 5 0 0 0 7 4l-4-4Z",
  };
  return <svg className="oauth-capability-icon" viewBox="0 0 24 24" aria-hidden="true"><path d={paths[name] ?? paths.data} /></svg>;
}
function serviceLogo(scope: string): ConnectionLogoId {
  const service = scope.slice("connector:".length);
  return service in serviceNames && service !== "whatsapp" ? service as ConnectionLogoId : "mcp";
}

function oauthIssuer(url: URL): string | undefined {
  const values = url.searchParams.getAll("oauth_issuer");
  if (values.length === 0) return isLocalDevelopmentOrigin(url.origin) ? url.origin : productionConnectApiOrigin;
  if (values.length !== 1) return undefined;
  const issuer = values[0]!;
  if (issuer === productionConnectApiOrigin || issuer === "https://nanocodex.gakonst.workers.dev") return issuer;
  return isLocalDevelopmentOrigin(url.origin) && issuer === url.origin ? issuer : undefined;
}

function isConsentRequest(value: unknown): value is ConsentRequest {
  if (!isRecord(value) || typeof value.client_id !== "string" || !value.client_id
    || typeof value.client_name !== "string" || !value.client_name || value.client_name.length > 200
    || typeof value.app_id !== "string" || !/^mcp:[A-Za-z0-9_-]{43}$/.test(value.app_id)
    || typeof value.app_origin !== "string" || typeof value.redirect_uri !== "string"
    || typeof value.resource !== "string" || typeof value.scope !== "string" || !value.scope
    || !Array.isArray(value.resources) || value.resources.length > 32
    || value.resources.some(resource => typeof resource !== "string" || resource.length > 512)
    || !Array.isArray(value.base_resources) || !isRecord(value.scope_resources)) return false;
  const scopes = value.scope.split(" ");
  const mappings = value.scope_resources;
  const allResources = value.resources;
  if (new Set(scopes).size !== scopes.length || scopes.some(scope => !scope || !Array.isArray(mappings[scope]))
    || Object.keys(mappings).some(scope => !scopes.includes(scope))) return false;
  const derived = [...value.base_resources, ...scopes.flatMap(scope => mappings[scope] as unknown[])];
  if (derived.some(resource => typeof resource !== "string" || !allResources.includes(resource))
    || value.resources.some(resource => !derived.includes(resource))) return false;
  try {
    const redirect = new URL(value.redirect_uri);
    const resource = new URL(value.resource);
    return redirect.origin === value.app_origin && !redirect.username && !redirect.password && !redirect.hash
      && (redirect.protocol === "https:" || (redirect.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(redirect.hostname)))
      && (resource.protocol === "https:" || (resource.protocol === "http:" && isLocalDevelopmentOrigin(resource.origin)))
      && value.resources.includes(`urn:nanocodex:app:${encodeURIComponent(value.app_id)}`)
      && value.resources.includes(`urn:nanocodex:origin:${encodeURIComponent(value.app_origin)}`)
      && value.resources.includes("urn:nanocodex:authorization:hosted");
  } catch { return false; }
}

function callbackUrl(value: string, expected: string): URL {
  const target = new URL(value);
  const registered = new URL(expected);
  if (target.origin !== registered.origin || target.pathname !== registered.pathname
    || target.username || target.password || target.hash
    || [...registered.searchParams.keys()].some(key =>
      JSON.stringify(target.searchParams.getAll(key)) !== JSON.stringify(registered.searchParams.getAll(key)))) {
    throw new Error("The authorization returned an unexpected callback address. Start a new connection from your MCP client.");
  }
  return target;
}

const serviceNames: Record<string, string> = {
  cloudflare: "Cloudflare", github: "GitHub", gmail: "Gmail", gdrive: "Google Drive",
  gcalendar: "Google Calendar", gtasks: "Google Tasks", gdocs: "Google Docs", gsheets: "Google Sheets",
  gslides: "Google Slides", gcontacts: "Google Contacts", slack: "Slack", x: "X",
  spotify: "Spotify", soundcloud: "SoundCloud", link: "Stripe Link", whatsapp: "WhatsApp",
};
function serviceLabel(scope: string): string {
  const name = scope.slice("connector:".length);
  return serviceNames[name] ?? name;
}
function capabilityLabel(group: string): string {
  return ({ agent: "Run agents", agents: "Agents", history: "Conversation history", memory: "Saved memory",
    data: "Application data", tools: "Tools" } as Record<string, string>)[group] ?? group;
}
function scopeAction(scope: string): string {
  const action = scope.split(":")[1] ?? scope;
  return ({ read: "Read", write: "Write", run: "Allow", use: "Allow", portability: "Export" } as Record<string, string>)[action] ?? action;
}
function scopeLabel(scope: string): string {
  if (requiredConnector(scope)) return `Use ${serviceLabel(scope)}`;
  if (scope === "agent:run") return "Run agents using your connected ChatGPT account";
  if (scope === "tools:use") return "Use tools authorized by this connection";
  return `${scopeAction(scope)} ${capabilityLabel(scope.split(":")[0]!).toLowerCase()}`;
}
function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : "The authorization is unavailable. Start a new connection from your MCP client.";
}

function requiredConnector(scope: string): string | undefined {
  return scope.startsWith("connector:") ? scope.slice("connector:".length) : undefined;
}
function scopeAvailable(scope: string, connectors: Readonly<Record<string, unknown>> | undefined): boolean {
  const capability = requiredConnector(scope);
  if (!capability) return true;
  const status = connectors?.[capability];
  return isRecord(status) && status.connected === true;
}
function responseDescription(body: unknown): string | undefined {
  if (!isRecord(body)) return undefined;
  const message = body.error_description ?? body.message;
  return typeof message === "string" && message.length > 0 && message.length <= 1_000 ? message : undefined;
}
