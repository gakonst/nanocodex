import { cloudflareTest } from "@cloudflare/vitest-pool-workers";
import { defineConfig } from "vitest/config";

const transientGoogleRevocations = new Set<string>();

const TEST_KEY = "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY";
const TEST_CHATGPT_EGRESS = `
export class ChatGptEgress {
  fetch(request) {
    const url = new URL(request.url);
    url.hostname = "chatgpt.com";
    return Response.json({
      url: url.href,
      credential: request.headers.get("authorization")?.startsWith("Bearer ")
        ? "chatgpt"
        : "missing",
      account: request.headers.get("chatgpt-account-id"),
      subject: request.headers.get("x-nanocodex-subject"),
      leaked: request.headers.get("x-should-not-forward"),
    }, { headers: { authorization: "Bearer reflected-provider-secret" } });
  }
}
`;

export default defineConfig({
  plugins: [
    cloudflareTest({
      wrangler: { configPath: "./wrangler.broker.jsonc" },
      miniflare: {
        bindings: {
          ENVIRONMENT: "test",
          CREDENTIAL_ENCRYPTION_KEY: TEST_KEY,
          ALLOW_LOCAL_CREDENTIAL_CLAIM: "true",
          LOCAL_CHATGPT_BOOTSTRAP: JSON.stringify({
            access_token: jwt({ exp: 4_102_444_800, marker: "local-access" }),
            refresh_token: "local-refresh-secret",
            account_id: "local-account",
            expires_at: 4_102_444_800_000,
          }),
          NANOCODEX_BROKER_PROBE_TOKEN: "probe-token-that-is-at-least-thirty-two-bytes",
          GITHUB_OAUTH_CLIENT_ID: "github-client-id",
          GITHUB_OAUTH_CLIENT_SECRET: "github-client-secret",
          GOOGLE_OAUTH_CLIENT_ID: "google-client-id",
          GOOGLE_OAUTH_CLIENT_SECRET: "google-client-secret",
          X_OAUTH_CLIENT_ID: "x-client-id",
          X_OAUTH_CLIENT_SECRET: "x-client-secret",
        },
        workers: [{
          name: "nanocodex",
          modules: true,
          script: TEST_CHATGPT_EGRESS,
          durableObjects: { CHATGPT_EGRESS: "ChatGptEgress" },
        }],
        outboundService: async (request) => {
          const url = new URL(request.url);
          if (request.method === "POST" && url.hostname === "github.com"
            && url.pathname === "/login/oauth/access_token") {
            const body = await request.clone().formData();
            if (body.get("grant_type") === "refresh_token") {
              if (body.get("refresh_token") === "github-revoked-refresh") {
                return Response.json({ error: "bad_refresh_token" }, { status: 400 });
              }
              return Response.json({
                access_token: "github-refreshed-access",
                expires_in: 28_800,
                refresh_token: "github-rotated-refresh",
                refresh_token_expires_in: 15_897_600,
                token_type: "bearer",
                scope: "repo,workflow",
              });
            }
            const code = String(body.get("code") ?? "");
            return Response.json({
              access_token: code === "github-code"
                ? "github-connector-access"
                : `github-${code.replace(/-code$/, "")}-access`,
              token_type: "bearer",
              scope: "repo,workflow",
              ...(code === "expired-code" ? {
                expires_in: 1,
                refresh_token: "github-expired-refresh",
                refresh_token_expires_in: 15_897_600,
              } : {}),
              ...(code === "revoked-refresh-code" ? {
                expires_in: 1,
                refresh_token: "github-revoked-refresh",
                refresh_token_expires_in: 15_897_600,
              } : {}),
              ...(code === "no-refresh-code" ? { expires_in: 1 } : {}),
            });
          }
          if (request.method === "DELETE" && url.hostname === "api.github.com"
            && url.pathname === "/applications/github-client-id/token") {
            if (request.headers.get("authorization")
              !== `Basic ${btoa("github-client-id:github-client-secret")}`) {
              return Response.json({ message: "Bad credentials" }, { status: 401 });
            }
            const body = await request.clone().json() as { access_token?: unknown };
            if (body.access_token === "github-revoke-failure-access") {
              return Response.json({ message: "Unavailable" }, { status: 503 });
            }
            return typeof body.access_token === "string" && body.access_token
              ? new Response(null, { status: 204 })
              : Response.json({ message: "Invalid" }, { status: 422 });
          }
          if (request.method === "GET" && url.hostname === "api.github.com"
            && url.pathname === "/user") {
            return Response.json({ id: 42, login: "nanocat", name: "Nano Cat" });
          }
          if (request.method === "POST" && url.hostname === "api.x.com"
            && url.pathname === "/2/oauth2/token") {
            const body = await request.clone().formData();
            const refresh = body.get("grant_type") === "refresh_token";
            const code = String(body.get("code") ?? "");
            const revocationFailure = code === "x-revocation-failure-code";
            const partialRevocation = code === "x-partially-revoked-code";
            const alreadyRevoked = code === "x-already-revoked-code";
            const revocationThrottled = code === "x-revocation-throttled-code";
            return Response.json({
              access_token: refresh ? "x-refreshed-access"
                : revocationFailure ? "x-revocation-failure-access"
                : partialRevocation ? "x-partially-revoked-access"
                : alreadyRevoked ? "x-already-revoked-access"
                : revocationThrottled ? "x-revocation-throttled-access"
                : "x-connector-access",
              ...(code === "x-no-refresh-code" ? {} : {
                refresh_token: revocationFailure
                  ? "x-revocation-failure-refresh"
                  : partialRevocation ? "x-partially-revoked-refresh"
                  : alreadyRevoked ? "x-already-revoked-refresh"
                  : revocationThrottled ? "x-revocation-throttled-refresh"
                  : "x-connector-refresh",
              }),
              expires_in: !refresh && code === "x-expiring-code" ? 1 : 7_200,
              token_type: "bearer",
              scope: code === "x-reduced-scope-code"
                ? "tweet.read users.read offline.access"
                : "tweet.read tweet.write users.read follows.read follows.write like.read like.write bookmark.read bookmark.write list.read list.write dm.read dm.write media.write offline.access",
            });
          }
          if (request.method === "POST" && url.hostname === "api.x.com"
            && url.pathname === "/2/oauth2/revoke") {
            const body = await request.clone().formData();
            const token = String(body.get("token") ?? "");
            if (request.headers.has("authorization") || token === "x-connector-access") {
              return Response.json({ error: "invalid revocation request" }, { status: 400 });
            }
            if (token === "x-partially-revoked-refresh"
              || token === "x-already-revoked-refresh"
              || token === "x-already-revoked-access") {
              return Response.json({ error: "invalid token" }, { status: 400 });
            }
            if (token === "x-revocation-throttled-refresh") {
              return Response.json({ error: "rate limited" }, { status: 429 });
            }
            if (token === "x-revocation-failure-refresh") {
              return Response.json({ error: "provider unavailable" }, { status: 503 });
            }
            return Response.json({ revoked: token });
          }
          if (request.method === "GET" && url.hostname === "api.x.com"
            && url.pathname === "/2/users/me") {
            return Response.json({
              data: { id: "2244994945", username: "nanocodex", name: "Nanocodex" },
            });
          }
          if (request.method === "POST" && url.hostname === "oauth2.googleapis.com"
            && url.pathname === "/token") {
            const body = await request.clone().formData();
            if (body.get("grant_type") === "refresh_token") {
              if (body.get("refresh_token") === "gmail-revoked-refresh") {
                return Response.json({ error: "invalid_grant" }, { status: 400 });
              }
              const drive = body.get("refresh_token") === "gdrive-connector-refresh";
              return Response.json({
                access_token: drive ? "gdrive-refreshed-access" : "gmail-refreshed-access",
                expires_in: 3_600,
                token_type: "Bearer",
              });
            }
            const code = String(body.get("code") ?? "");
            const sharedAccount = code.endsWith("-shared-account-code");
            const drive = code === "gdrive-code" || code === "gdrive-shared-account-code";
            const expiring = body.get("code") === "gmail-expiring-code";
            const revoked = body.get("code") === "gmail-revoked-code";
            const revokeFailure = body.get("code") === "gmail-revoke-failure-code";
            return Response.json({
              access_token: sharedAccount
                ? drive ? "gdrive-shared-account-access" : "gmail-shared-account-access"
                : drive ? "gdrive-connector-access" : "gmail-connector-access",
              ...(body.get("code") === "gmail-no-refresh-code" ? {} : {
                refresh_token: drive
                  ? "gdrive-connector-refresh"
                  : revoked
                    ? "gmail-revoked-refresh"
                    : body.get("code") === "gmail-revoke-once-code"
                      ? "gmail-revoke-once-refresh"
                    : revokeFailure ? "gmail-revoke-failure-refresh" : "gmail-connector-refresh",
              }),
              expires_in: expiring || revoked || body.get("code") === "gmail-no-refresh-code"
                ? 1 : 3_600,
              token_type: "Bearer",
              scope: drive
                ? "openid email profile https://www.googleapis.com/auth/drive"
                : "openid email https://mail.google.com/",
            });
          }
          if (request.method === "POST" && url.hostname === "oauth2.googleapis.com"
            && url.pathname === "/revoke") {
            const body = await request.clone().formData();
            const token = String(body.get("token") ?? "");
            if (token === "gmail-revoke-failure-refresh") {
              return Response.json({ error: "temporarily_unavailable" }, { status: 503 });
            }
            if (token === "gmail-revoke-once-refresh"
              && !transientGoogleRevocations.has(token)) {
              transientGoogleRevocations.add(token);
              return Response.json({ error: "temporarily_unavailable" }, { status: 503 });
            }
            return token
              ? new Response(null, { status: 200 })
              : Response.json({ error: "invalid_request" }, { status: 400 });
          }
          if (request.method === "GET" && url.hostname === "openidconnect.googleapis.com"
            && url.pathname === "/v1/userinfo") {
            const authorization = request.headers.get("authorization");
            const sharedAccount = authorization?.endsWith("shared-account-access") === true;
            const drive = authorization === "Bearer gdrive-connector-access"
              || authorization === "Bearer gdrive-shared-account-access";
            return Response.json({
              sub: sharedAccount
                ? "google-shared-account"
                : drive ? "google-drive-account" : "google-gmail-account",
              email: sharedAccount
                ? "shared@example.test"
                : drive ? "drive@example.test" : "mail@example.test",
              email_verified: true,
              name: drive ? "Drive User" : "Mail User",
            });
          }
          if ((url.hostname === "api.github.com"
              || url.hostname === "gmail.googleapis.com"
              || url.hostname === "www.googleapis.com"
              || url.hostname === "api.x.com")) {
            const authorization = request.headers.get("authorization") ?? "";
            if (url.searchParams.has("redirect")) {
              return new Response(null, {
                status: 302,
                headers: { location: "https://attacker.example/collect" },
              });
            }
            if (url.searchParams.has("oversize")) {
              return new Response("bounded", { headers: { "content-length": "9000000" } });
            }
            if (url.searchParams.has("reflect_credential")) {
              return Response.json({ reflected: authorization });
            }
            if (url.searchParams.has("revoked")) {
              return Response.json({ message: "Bad credentials" }, { status: 401 });
            }
            const account = authorization === "Bearer github-alpha-access" ? "alpha"
              : authorization === "Bearer github-beta-access" ? "beta"
              : authorization === "Bearer github-refreshed-access" ? "github-refreshed"
              : authorization === "Bearer gmail-refreshed-access" ? "gmail-refreshed"
              : authorization === "Bearer x-refreshed-access" ? "x-refreshed"
              : authorization.startsWith("Bearer ") ? "connected" : "missing";
            return Response.json({
              account,
              host: url.hostname,
              path: url.pathname,
              method: request.method,
              body: request.body ? await request.text() : null,
              content_type: request.headers.get("content-type"),
              caller_cookie: request.headers.has("cookie"),
              caller_proxy_credential: request.headers.has("proxy-authorization"),
              subject: request.headers.get("x-nanocodex-subject"),
            }, {
              headers: {
                authorization,
                "set-cookie": "provider-secret=cookie",
              },
            });
          }
          if (["mcp-fixture.nanocodex.dev", "mcp.linear.app", "mcp-standard.nanocodex.dev"].includes(url.hostname)
            && request.method === "GET" && url.pathname === "/mcp") {
            const authorization = request.headers.get("authorization");
            if (!authorization) {
              if (url.hostname === "mcp-standard.nanocodex.dev") {
                return Response.json({ error: "method_not_allowed" }, { status: 405 });
              }
              return new Response(null, {
                status: 401,
                headers: {
                  "www-authenticate": `Bearer resource_metadata="${url.origin}/.well-known/oauth-protected-resource/mcp"`,
                },
              });
            }
            if (authorization === "Bearer mcp-stale-access") {
              return Response.json({ error: "expired" }, { status: 401 });
            }
            const lastEventId = request.headers.get("last-event-id");
            if (lastEventId === "reflect-header") {
              return new Response("blocked", {
                headers: {
                  "content-type": "text/event-stream",
                  "mcp-session-id": "mcp-access-token",
                },
              });
            }
            if (lastEventId === "reflect-status") {
              return new Response("safe", { status: 299, statusText: "mcp-access-token" });
            }
            if (lastEventId === "reflect-body") {
              const encoder = new TextEncoder();
              return new Response(new ReadableStream({
                start(controller) {
                  controller.enqueue(encoder.encode('{"secret":"mcp-'));
                  controller.enqueue(encoder.encode('access-token"}'));
                  controller.close();
                },
              }), { headers: { "content-type": "application/json" } });
            }
            return Response.json({
              authorized: authorization === "Bearer mcp-refreshed-access"
                ? "refreshed"
                : "connected",
              method: request.method,
              accept: request.headers.get("accept"),
              content_type: request.headers.get("content-type"),
              protocol_version: request.headers.get("mcp-protocol-version"),
              session_id: request.headers.get("mcp-session-id"),
              last_event_id: lastEventId,
              caller_header: request.headers.get("x-should-not-forward"),
              body: request.body ? await request.text() : null,
            }, {
              headers: {
                "mcp-session-id": "upstream-session",
                "retry-after": "3",
                "x-should-not-forward": "upstream-private",
              },
            });
          }
          if (["mcp-fixture.nanocodex.dev", "mcp.linear.app", "mcp-standard.nanocodex.dev"].includes(url.hostname)
            && request.method === "POST" && url.pathname === "/mcp") {
            const authorization = request.headers.get("authorization");
            if (authorization === "Bearer mcp-stale-access") {
              return Response.json({ error: "expired" }, { status: 401 });
            }
            if (request.headers.get("last-event-id") === "reflect-old-body") {
              return Response.json({ reflected: "mcp-stale-access" });
            }
            return Response.json({
              authorized: authorization === "Bearer mcp-refreshed-access"
                ? "refreshed"
                : "connected",
              method: request.method,
              accept: request.headers.get("accept"),
              content_type: request.headers.get("content-type"),
              protocol_version: request.headers.get("mcp-protocol-version"),
              session_id: request.headers.get("mcp-session-id"),
              last_event_id: request.headers.get("last-event-id"),
              caller_header: request.headers.get("x-should-not-forward"),
              body: request.body ? await request.text() : null,
            }, {
              headers: {
                "mcp-session-id": "upstream-session",
                "retry-after": "3",
                "x-should-not-forward": "upstream-private",
              },
            });
          }
          if (["mcp-fixture.nanocodex.dev", "mcp.linear.app", "mcp-standard.nanocodex.dev"].includes(url.hostname)
            && request.method === "DELETE" && url.pathname === "/mcp") {
            return new Response(null, { status: 204, headers: { "mcp-session-id": "deleted" } });
          }
          if (["mcp-fixture.nanocodex.dev", "mcp.linear.app", "mcp-standard.nanocodex.dev"].includes(url.hostname)
            && request.method === "GET"
            && url.pathname === "/.well-known/oauth-protected-resource/mcp") {
            return Response.json({
              resource: `${url.origin}/mcp`,
              authorization_servers: ["https://mcp-auth.nanocodex.dev"],
              scopes_supported: ["read", "write"],
            });
          }
          if (url.hostname === "mcp-auth.nanocodex.dev" && request.method === "GET"
            && url.pathname === "/.well-known/oauth-authorization-server") {
            return Response.json({
              issuer: "https://mcp-auth.nanocodex.dev/",
              authorization_endpoint: "https://mcp-auth.nanocodex.dev/authorize",
              token_endpoint: "https://mcp-auth.nanocodex.dev/token",
              registration_endpoint: "https://mcp-auth.nanocodex.dev/register",
              revocation_endpoint: "https://mcp-auth.nanocodex.dev/revoke",
              code_challenge_methods_supported: ["S256"],
              scopes_supported: ["read", "write"],
            });
          }
          if (url.hostname === "mcp-auth.nanocodex.dev" && request.method === "POST"
            && url.pathname === "/register") {
            const registration = await request.json() as Record<string, unknown>;
            return registration.token_endpoint_auth_method === "none"
              ? Response.json({
                  client_id: "mcp-dynamic-client",
                  client_secret: "mcp-dynamic-secret",
                  token_endpoint_auth_method: "client_secret_post",
                }, { status: 201 })
              : Response.json({ error: "invalid_client_metadata" }, { status: 400 });
          }
          if (url.hostname === "mcp-auth.nanocodex.dev" && request.method === "POST"
            && url.pathname === "/token") {
            const body = await request.formData();
            if (body.get("client_id") !== "mcp-dynamic-client"
              || body.get("client_secret") !== "mcp-dynamic-secret"
              || body.get("resource") === null) {
              return Response.json({ error: "invalid_client" }, { status: 401 });
            }
            if (body.get("grant_type") === "refresh_token") {
              return body.get("refresh_token") === "mcp-refresh-token"
                ? Response.json({
                    access_token: "mcp-refreshed-access",
                    refresh_token: "mcp-refresh-rotated",
                    token_type: "Bearer",
                    expires_in: 3_600,
                    scope: "read",
                  })
                : Response.json({ error: "invalid_grant" }, { status: 400 });
            }
            if (!body.get("code_verifier") || body.get("redirect_uri") === null) {
              return Response.json({ error: "invalid_grant" }, { status: 400 });
            }
            return Response.json({
              access_token: body.get("code") === "refresh-once"
                ? "mcp-stale-access"
                : "mcp-access-token",
              refresh_token: "mcp-refresh-token",
              token_type: "Bearer",
              expires_in: 3_600,
              scope: "read",
            });
          }
          if (url.hostname === "mcp-auth.nanocodex.dev" && request.method === "POST"
            && url.pathname === "/revoke") {
            const body = await request.formData();
            return body.get("token")
              ? new Response(null, { status: 200 })
              : Response.json({ error: "invalid_request" }, { status: 400 });
          }
          if (request.method === "POST" && url.pathname.endsWith("/deviceauth/usercode")) {
            return Response.json({
              device_auth_id: "device-secret",
              user_code: "ABCD-EFGH",
              interval: "1",
            });
          }
          if (request.method === "POST" && url.pathname.endsWith("/deviceauth/token")) {
            return Response.json({
              authorization_code: "authorization-secret",
              code_challenge: "challenge-secret",
              code_verifier: "verifier-secret",
            });
          }
          if (request.method === "POST" && url.pathname.endsWith("/oauth/token")) {
            const contentType = request.headers.get("content-type") ?? "";
            if (contentType.startsWith("application/x-www-form-urlencoded")) {
              return Response.json({
                access_token: jwt({ exp: 4_102_444_800, marker: "chatgpt-access" }),
                refresh_token: "chatgpt-refresh-secret",
                id_token: jwt({
                  "https://api.openai.com/auth": {
                    chatgpt_account_id: "chatgpt-account",
                    chatgpt_account_is_fedramp: false,
                  },
                }),
              });
            }
            return Response.json({
              access_token: jwt({ exp: 4_102_444_800, marker: "chatgpt-refreshed" }),
              refresh_token: "chatgpt-refresh-rotated",
            });
          }
          if (url.hostname === "api.openai.com" || url.hostname === "chatgpt.com") {
            const authorization = request.headers.get("authorization");
            return Response.json({
              url: request.url,
              credential: authorization === "Bearer sk-user-a-secret"
                ? "openai-a"
                : authorization === "Bearer sk-user-b-secret"
                ? "openai-b"
                : authorization?.startsWith("Bearer ")
                ? "chatgpt"
                : "missing",
              account: request.headers.get("chatgpt-account-id"),
              subject: request.headers.get("x-nanocodex-subject"),
              leaked: request.headers.get("x-should-not-forward"),
            }, { headers: { authorization: "Bearer reflected-provider-secret" } });
          }
          return new Response("unexpected outbound request", { status: 599 });
        },
      },
    }),
  ],
  test: {
    include: ["test/**/*.test.ts"],
    testTimeout: 15_000,
  },
});

function jwt(payload: Record<string, unknown>): string {
  const encode = (value: unknown) => Buffer.from(JSON.stringify(value)).toString("base64url");
  return `${encode({ alg: "none" })}.${encode(payload)}.test`;
}
