import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { connectorCapabilities, publicConnectorStatus } from "../../../connect-api/src/connectorPolicy.mts";
import { fileURLToPath } from "node:url";

// Only external account/SMS services are synthetic. Components and fetch transport are real.
export default defineConfig({
  root: fileURLToPath(new URL(".", import.meta.url)),
  plugins: [react(), {
    name: "synthetic-account-transport",
    configureServer(server) {
      let granted: string[] = [];
      const sessions = new Map<string, string>();
      let nextSession = 0;
      const brokerStatuses = () => Object.fromEntries(connectorCapabilities.map(capability => [capability, publicConnectorStatus({ connected: true, connections: [{ id: "c".repeat(43), label: "Synthetic account", capabilities: [capability] }] })]));
      const connectors = () => ({ ...brokerStatuses(), github: { connected: true, label: "atlas-demo" }, gmail: { connected: granted.includes("gmail"), label: granted.includes("gmail") ? "alex@example.com" : undefined }, gcalendar: { connected: granted.includes("gcalendar") } });
      server.middlewares.use(async (req, res, next) => {
        if (!req.url?.startsWith("/v1/") && !req.url?.startsWith("/oauth/requests/")) return next();
        let raw = "";
        for await (const chunk of req) raw += chunk;
        const body = raw ? JSON.parse(raw) : {};
        let status = 200;
        let result: unknown;
        // Fixture-only HttpOnly session. Tests choose server state, never client
        // account claims; this cookie is unrelated to production credentials.
        const sessionId = /(?:^|; )fixture-session=([^;]+)/.exec(req.headers.cookie ?? "")?.[1];
        const session = sessions.get(sessionId ?? "") ?? "anonymous";
        const address = session === "persistent-other" ? "0x2222222222222222222222222222222222222222" : "0x1111111111111111111111111111111111111111";
        const setSession = (state: string) => {
          const id = String(++nextSession);
          sessions.set(id, state);
          res.setHeader("set-cookie", `fixture-session=${id}; HttpOnly; SameSite=Lax; Path=/`);
        };
        const oauth = /^\/oauth\/requests\/([A-Za-z0-9_-]{43})(?:\/(approve|deny))?$/.exec(req.url ?? "");
        if (oauth) {
          const appId = `mcp:${"c".repeat(43)}`;
          const redirect = "http://127.0.0.1:4198/oauth-callback?registered=kept";
          const appOrigin = new URL(redirect).origin;
          if (oauth[1] === "z".repeat(43)) {
            status = 410; result = { error: "invalid_request" };
          } else if (!oauth[2]) {
            const baseResources = [
              `urn:nanocodex:app:${encodeURIComponent(appId)}`,
              `urn:nanocodex:origin:${encodeURIComponent(appOrigin)}`,
              "urn:nanocodex:authorization:hosted", "urn:nanocodex:agent:run",
            ];
            const scopeResources: Record<string, string[]> = {
              "agent:run": ["urn:nanocodex:connector:chatgpt", "urn:nanocodex:agent:output:final", "urn:nanocodex:agent:output:actions"],
              "history:read": ["urn:nanocodex:history:read"],
              "connector:gmail": ["urn:nanocodex:connector:gmail"],
              "connector:slack": ["urn:nanocodex:connector:slack"],
            };
            if (oauth[1] === "f".repeat(43)) {
              for (const scope of ["memory:read", "memory:write", "data:read", "data:write", "connector:cloudflare", "connector:github", "connector:gdrive", "connector:gcalendar", "connector:gtasks", "connector:gdocs", "connector:gsheets", "connector:gslides", "connector:gcontacts", "connector:x", "connector:spotify", "connector:soundcloud", "connector:link"]) scopeResources[scope] = [`urn:nanocodex:${scope}`];
            }
            result = {
              client_id: "c".repeat(43), client_name: "Synthetic MCP Client", app_id: appId,
              app_origin: appOrigin, redirect_uri: redirect,
              scope: Object.keys(scopeResources).join(" "), resource: "https://nanocodex-connect-api.gakonst.workers.dev/mcp",
              base_resources: baseResources, scope_resources: scopeResources,
              resources: [...baseResources, ...Object.values(scopeResources).flat()],
            };
          } else if (oauth[2] === "approve" && body.code !== "s".repeat(43)) {
            status = 403; result = { error: "invalid_approval" };
          } else {
            result = { redirect_uri: oauth[1] === "t".repeat(43)
              ? "https://unexpected.example/callback?code=bad"
              : `${redirect}&${oauth[2] === "approve" ? "code=synthetic-code" : "error=access_denied"}&state=original-state` };
          }
        } else switch (new URL(req.url, "http://fixture.local").pathname) {
          case "/v1/fixture/session": setSession(body.state); result = { ok: true }; break;
          case "/v1/me":
            if (session === "expired") { status = 401; result = { error: "reauthentication_required" }; }
            else if (session === "unavailable") { status = 503; result = { error: "unavailable" }; }
            else result = { user: {
              id: "synthetic-user", persistent: session !== "anonymous",
              ...(session !== "anonymous" && session !== "missing-address" ? { address } : {}),
            } };
            break;
          case "/v1/auth/logout": setSession("anonymous"); result = { ok: true }; break;
          case "/v1/auth/sms/start":
            await new Promise(resolve => setTimeout(resolve, 120));
            status = body.phone === "+12025550000" ? 503 : 200;
            result = status === 503 ? { error: "sms_delivery_failed" } : { challenge_id: "synthetic-sms-challenge", expires_in: 600 };
            break;
          case "/v1/auth/sms/verify":
            await new Promise(resolve => setTimeout(resolve, 120));
            status = body.code === "123456" ? 200 : 400;
            result = status === 200 ? { user: { id: "synthetic-user", address } } : { error: "invalid_or_expired_otp" };
            if (status === 200) setSession("persistent");
            break;
          case "/v1/connect/hosted-authorization/authorize":
            if (session === "delayed-authorization") await new Promise(resolve => setTimeout(resolve, 500));
            if (!["persistent", "persistent-other", "delayed-authorization", "delayed-exchange"].includes(session)) { status = 401; result = { error: "unauthorized" }; }
            else if (body.account_address !== address) { status = 403; result = { error: "account_address_mismatch" }; }
            else if (body.resources?.includes("urn:nanocodex:history:read")) { status = 400; result = { message: "This permission is unavailable for this account." }; }
            else result = { code: "s".repeat(43) };
            break;
          case "/v1/connectors/google": result = { authorization_url: "http://modal.nanocodex.localhost:4198/provider.html" }; break;
          case "/v1/connectors": result = { connectors: req.headers.referer?.includes("oauth_request=") ? { ...connectors(), chatgpt: undefined, gmail: { connected: true }, slack: { connected: false } } : connectors() }; break;
          case "/v1/fixture/google-complete": granted = body.capabilities; result = { ok: true }; break;
          case "/v1/hosted-authorizations":
            if (session === "delayed-exchange") await new Promise(resolve => setTimeout(resolve, 500));
            granted = []; result = {
            account_address: address, approval_id: "a".repeat(43), token: "synthetic-token",
            connectors: body.resources?.includes("urn:nanocodex:connectors:github,gmail,gcalendar") ? connectors() : brokerStatuses(), mcp_connections: [], profile: { linked: true },
          }; break;
          default: status = 404; result = { error: "unexpected_fixture_endpoint" };
        }
        res.statusCode = status;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(result));
      });
    },
  }],
  resolve: { dedupe: ["react", "react-dom"] },
  server: { host: "127.0.0.1", port: 4198, strictPort: true, allowedHosts: ["modal.nanocodex.localhost"] },
});
