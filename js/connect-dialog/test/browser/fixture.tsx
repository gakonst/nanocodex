import { App } from "../../src/DialogApp";
import { appearanceFromSearch } from "../../src/appearance";
import { StrictMode, useMemo, useState } from "react";
import { createRoot } from "react-dom/client";
import { ConnectOnboarding, type ConnectRequest } from "nanocodex-connect-ui/App";
import "nanocodex-connect-ui/styles.css";

const sshImport = new URLSearchParams(location.search).has("ssh-import");
const appId = sshImport ? "nanocodex-cli" : "modal-journey";
const appOrigin = sshImport ? "https://cli.nanocodex.xyz" : "http://atlas.nanocodex.localhost";
const request: ConnectRequest = {
  type: "walletConnect",
  id: "synthetic-modal-request",
  appId,
  origin: appOrigin,
  rpc: {
    method: "wallet_connect",
    params: [{ capabilities: { auth: {
      url: `${window.location.origin}/v1/connect/auth`,
      resources: [
        `urn:nanocodex:app:${appId}`,
        ...(sshImport ? [
          `urn:nanocodex:credential-import:ssh:pem-v1:sha256:${"a".repeat(43)}`,
          `urn:nanocodex:ssh-target:synthetic-lab:server.example.com:2222:deploy:SHA256%3A${"a".repeat(43)}`,
        ] : []),
        `urn:nanocodex:origin:${encodeURIComponent(appOrigin)}`,
        ...(new URLSearchParams(window.location.search).has("fresh-auth") ? [] : ["urn:nanocodex:authorization:hosted"]),
        ...(new URLSearchParams(window.location.search).has("spending") ? ["urn:nanocodex:mpp:machusd:spend"] : []),
        "urn:nanocodex:agent:run",
        ...(new URLSearchParams(window.location.search).has("focused") ? ["urn:nanocodex:connector-focus:gmail"] : []),
        ...(new URLSearchParams(window.location.search).has("connections") ? [
          "urn:nanocodex:connectors:github,gmail,gcalendar",
          "urn:nanocodex:agent:output:final",
          "urn:nanocodex:agent:output:actions",
        ] : []),
      ],
    } } }],
  },
};

function Fixture() {
  return new URLSearchParams(location.search).has("oauth_request") ? <App /> : <WalletFixture />;
}

function WalletFixture() {
  const [outcome, setOutcome] = useState<string>();
  const [activeRequest, setActiveRequest] = useState(request);
  const [open, setOpen] = useState(true);
  const host = useMemo(() => ({
    async respond(result: unknown) {
      (window as any).__hostReceipt = { kind: "approved", result };
      setOutcome("Request approved");
    },
    async reject(error?: unknown) {
      (window as any).__hostReceipt = { kind: "cancelled", error: String(error) };
      setOutcome("Request cancelled");
    },
  }), []);
  const preset = new URLSearchParams(location.search).get("appearance");
  const appearance = preset === "brand" ? {theme: "dark" as const, accentColor: "#c4b5fd", borderRadius: 6, fontFamily: "system-ui"}
    : preset === "invalid" ? {accentColor: "url(https://invalid.example)", borderRadius: -1, fontFamily: "bad; display:none"} : appearanceFromSearch(location.search);
  return outcome ? <p role="status">{outcome}</p> : <>
    {new URLSearchParams(location.search).has("replace-request") ? <button type="button" onClick={() => setActiveRequest({ ...request, id: "replacement-request" })}>Replace request</button> : null}
    {new URLSearchParams(location.search).has("unmount") ? <button type="button" onClick={() => setOpen(false)}>Close Connect</button> : null}
    {open ? <ConnectOnboarding appearance={appearance} host={host} presentation={new URLSearchParams(location.search).has("wizard") ? "wizard" : "dialog"} request={activeRequest} /> : <p role="status">Connect closed</p>}</>;
}

createRoot(document.getElementById("root")!).render(new URLSearchParams(location.search).has("strict") ? <StrictMode><Fixture /></StrictMode> : <Fixture />);
