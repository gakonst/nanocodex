import { env } from "cloudflare:workers";
import { runInDurableObject, SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";

import {
  GOOGLE_CAPABILITIES,
  GOOGLE_PROVIDER,
  buildGoogleAuthorizationUrl,
  decodeGoogleTokenResponse,
  googleCapabilities,
} from "../src/connectors/google";
import {
  SLACK_PROVIDER,
  buildSlackAuthorizationUrl,
  decodeSlackTokenResponse,
} from "../src/connectors/slack";
import type { EgressEnv } from "../src/egress";
import { UserConnectorBroker } from "../src/connector-broker";
import { CredentialVault, type EncryptedEnvelope } from "../src/credential-vault";

const workerEnv = env as unknown as EgressEnv;
const redirectUri = "https://nanocodex.test/v1/connectors/callback";

describe("provider-neutral connector identities", () => {
  it("streams authenticated Git packs beyond 16 MiB and fences a disconnected connection", async () => {
    const user = "large-git-egress";
    const subject = "L".repeat(43);
    const connection = await connect(user, "github", "github-code");
    await bindSubject(subject, user);
    const headers = {
      authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
      "x-nanocodex-subject": subject,
      "x-nanocodex-connector-connection": connection,
    };
    const metadata = await (await SELF.fetch("https://api.github.com/repos/fixture/large", { headers })).json() as { head: string };
    const refs = await SELF.fetch("https://github.com/fixture/large.git/info/refs?service=git-upload-pack", { headers });
    expect(refs.status).toBe(200);
    expect(await refs.text()).toContain(metadata.head);
    const want = `want ${metadata.head} side-band-64k ofs-delta\n`;
    const body = `${(want.length + 4).toString(16).padStart(4, "0")}${want}00000009done\n`;
    const response = await SELF.fetch("https://github.com/fixture/large.git/git-upload-pack", {
      method: "POST", headers: { ...headers, "content-type": "application/x-git-upload-pack-request" }, body,
    });
    expect(response.status).toBe(200);
    const reader = response.body!.getReader();
    let bytes = 0;
    for (;;) { const part = await reader.read(); if (part.done) break; bytes += part.value.byteLength; }
    expect(bytes).toBeGreaterThan(16 * 1024 * 1024);
    expect((await SELF.fetch(`https://broker.test/users/${user}/connectors/github/connections/${connection}`, {
      method: "DELETE",
    })).status).toBe(204);
    const revoked = await SELF.fetch("https://github.com/fixture/large.git/info/refs?service=git-upload-pack", { headers });
    expect(revoked.status).toBe(404);
    expect(await revoked.json()).toEqual({ error: "connector_connection_not_found" });
  }, 60_000);

  it("requests the full Google Workspace catalog while decoding partial consent", () => {
    const authorization = buildGoogleAuthorizationUrl({
      clientId: "client", redirectUri, state: "state", codeChallenge: "A".repeat(43),
    });
    expect(authorization.searchParams.get("scope")?.split(" ")).toEqual(GOOGLE_PROVIDER.scopes);
    expect(Object.keys(GOOGLE_CAPABILITIES)).toEqual([
      "gmail", "gdrive", "gcalendar", "gtasks", "gdocs", "gsheets", "gslides", "gcontacts",
    ]);
    const token = decodeGoogleTokenResponse({
      access_token: "secret-access",
      refresh_token: "secret-refresh",
      expires_in: 3_600,
      token_type: "Bearer",
      scope: `openid email ${GOOGLE_CAPABILITIES.gmail} ${GOOGLE_CAPABILITIES.gcalendar}`,
    });
    expect(googleCapabilities(token.scopes)).toEqual(["gmail", "gcalendar"]);
  });

  it("projects one Google identity into each granted capability and selects among identities", async () => {
    const user = "multi-google-identities";
    const alpha = await connect(user, "google", "google-alpha-code");
    const beta = await connect(user, "google", "google-beta-code");
    expect(alpha).toMatch(/^[A-Za-z0-9_-]{43}$/);
    expect(beta).toMatch(/^[A-Za-z0-9_-]{43}$/);
    expect(beta).not.toBe(alpha);

    const status = await connectorStatus(user);
    expect(status.gmail.connected).toBe(true);
    expect(status.gmail.connections.map((connection) => connection.id)).toEqual([alpha, beta]);
    expect(status.gdrive.connections.map((connection) => connection.id)).toEqual([alpha]);
    expect(status.gcalendar.connections.map((connection) => connection.id)).toEqual([beta]);
    expect(status.gdocs).toEqual({ connected: false, connections: [] });
    expect(status.gmail.connections[0]).toMatchObject({
      id: alpha,
      label: "alpha@example.test",
      account_id: "google-alpha-account",
      capabilities: ["gmail", "gdrive"],
    });

    const broker = workerEnv.USER_CONNECTORS.getByName(user);
    const ambiguous = await broker.fetch("https://gmail.googleapis.com/gmail/v1/users/me/messages");
    expect(ambiguous.status).toBe(409);
    expect(await ambiguous.json()).toEqual({ error: "connector_connection_required" });
    const selected = await broker.fetch("https://gmail.googleapis.com/gmail/v1/users/me/messages", {
      headers: { "x-nanocodex-connector-connection": beta },
    });
    expect(selected.status).toBe(200);
    expect(await selected.json()).toMatchObject({ account: "google-beta" });

    const removed = await SELF.fetch(
      `https://broker.test/users/${user}/connectors/google/connections/${alpha}`,
      { method: "DELETE" },
    );
    expect(removed.status).toBe(204);
    const after = await connectorStatus(user);
    expect(after.gmail.connections.map((connection) => connection.id)).toEqual([beta]);
    expect(after.gdrive).toEqual({ connected: false, connections: [] });
  });

  it("allows the managed Google Calendar and People route matrix", async () => {
    const user = "google-route-matrix";
    const subject = "R".repeat(43);
    const connection = await connect(user, "google", "google-routes-code");
    await bindSubject(subject, user);

    for (const url of [
      "https://calendar.googleapis.com/calendar/v3/calendars/primary/events",
      "https://people.googleapis.com/v1/people:searchContacts",
      "https://people.googleapis.com/v1/otherContacts:search",
    ]) {
      const response = await SELF.fetch(url, { headers: {
        authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
        "x-nanocodex-subject": subject,
        "x-nanocodex-connector-connection": connection,
      } });
      expect(response.status, url).toBe(200);
      expect(await response.json()).toMatchObject({ host: new URL(url).hostname });
    }
  });

  it("stores and independently selects two Slack users in one workspace", async () => {
    const user = "multi-slack-identities";
    const first = await connect(user, "slack", "slack-a-code");
    const second = await connect(user, "slack", "slack-b-code");
    expect(first).toMatch(/^[A-Za-z0-9_-]{43}$/);
    expect(second).toMatch(/^[A-Za-z0-9_-]{43}$/);

    const status = await connectorStatus(user);
    expect(status.slack.connections).toEqual([
      expect.objectContaining({
        id: first, label: "Shared Workspace (UA)", account_id: "TSHARED:UA",
      }),
      expect.objectContaining({
        id: second, label: "Shared Workspace (UB)", account_id: "TSHARED:UB",
      }),
    ]);
    expect(status.slack.connections[0]!.id).not.toBe("TSHARED");
    const publicJson = JSON.stringify(status);
    expect(publicJson).not.toContain("slack-a-access");
    expect(publicJson).not.toContain("slack-b-access");

    const broker = workerEnv.USER_CONNECTORS.getByName(user);
    const selectedFirst = await broker.fetch("https://slack.com/api/chat.postMessage", {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "x-nanocodex-connector-connection": first,
      },
      body: JSON.stringify({ channel: "C1", text: "from first" }),
    });
    expect(await selectedFirst.json()).toMatchObject({ account: "slack-a" });
    const selectedSecond = await broker.fetch("https://slack.com/api/chat.postMessage", {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "x-nanocodex-connector-connection": second,
      },
      body: JSON.stringify({ channel: "C1", text: "hello" }),
    });
    expect(await selectedSecond.json()).toMatchObject({ account: "slack-b" });

    const modelRevocation = await broker.fetch("https://slack.com/api/auth.revoke", {
      method: "POST",
      headers: { "x-nanocodex-connector-connection": first },
    });
    expect(modelRevocation.status).toBe(403);
    expect(await modelRevocation.json()).toEqual({ error: "destination_denied" });
    const subject = "S".repeat(43);
    await bindSubject(subject, user);
    const outerRevocation = await SELF.fetch("https://slack.com/api/auth.revoke", {
      method: "POST",
      headers: {
        authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
        "x-nanocodex-subject": subject,
        "x-nanocodex-connector-connection": first,
      },
    });
    expect(outerRevocation.status).toBe(403);
    expect(await outerRevocation.json()).toEqual({ error: "destination_denied" });
    expect((await connectorStatus(user)).slack.connections).toHaveLength(2);

    await runInDurableObject(broker, async (_instance: UserConnectorBroker, state) => {
      const raw = JSON.stringify(await state.storage.get("connector-state"));
      expect(raw).toContain("ciphertext");
      expect(raw).not.toContain("slack-a-access");
      expect(raw).not.toContain("slack-b-access");
    });
  });

  it("coalesces encrypted same-account Gmail and Drive grants into one revocable identity", async () => {
    const user = "legacy-connector-state";
    const stub = workerEnv.USER_CONNECTORS.getByName(user);
    await stub.fetch("https://connectors.internal/v1/status");
    await runInDurableObject(stub, async (_instance: UserConnectorBroker, state) => {
      const vault = new CredentialVault(workerEnv, `connectors/${state.id.toString()}`);
      const legacy = {
        version: 1,
        connectors: {
          gmail: {
            accessToken: "legacy-access-secret",
            refreshToken: "legacy-refresh-secret",
            expiresAt: Date.now() + 60_000,
            scopes: ["openid", "email", GOOGLE_CAPABILITIES.gmail],
            accountId: "legacy-google-account",
            label: "legacy@example.test",
            connectedAt: Date.now() - 1_000,
          },
          gdrive: {
            accessToken: "legacy-drive-access-secret",
            refreshToken: "legacy-drive-refresh-secret",
            expiresAt: Date.now() + 60_000,
            // Google incremental consent returns the complete granted scope set
            // on the newest token; migration must project exactly this list.
            scopes: [
              "openid",
              "email",
              GOOGLE_CAPABILITIES.gmail,
              GOOGLE_CAPABILITIES.gdrive,
            ],
            accountId: "legacy-google-account",
            label: "legacy@example.test",
            connectedAt: Date.now(),
          },
        },
        pending: {},
      };
      await state.storage.put("connector-state", {
        envelope: await vault.seal(legacy),
      } satisfies { envelope: EncryptedEnvelope });
      const raw = JSON.stringify(await state.storage.get("connector-state"));
      expect(raw).toContain("ciphertext");
      expect(raw).not.toContain("legacy-access-secret");
      expect(raw).not.toContain("legacy-refresh-secret");
      expect(raw).not.toContain("legacy-drive-access-secret");
      expect(raw).not.toContain("legacy-drive-refresh-secret");
    });

    // An ordinary rejected request exercises the durable-state recovery reader.
    await stub.fetch("https://connectors.internal/v1/google/start", { method: "POST" });
    const status = await stub.fetch("https://connectors.internal/v1/status");
    const connectors = (await status.json<{ connectors: PublicStatus }>()).connectors;
    expect(connectors.gmail.connections).toEqual([
      expect.objectContaining({
        id: expect.stringMatching(/^[A-Za-z0-9_-]{43}$/),
        label: "legacy@example.test",
        account_id: "legacy-google-account",
        capabilities: ["gmail", "gdrive"],
      }),
    ]);
    expect(connectors.gdrive.connections).toEqual(connectors.gmail.connections);
    await runInDurableObject(stub, async (_instance: UserConnectorBroker, state) => {
      const row = await state.storage.get<{ envelope: EncryptedEnvelope }>("connector-state");
      const raw = JSON.stringify(row);
      expect(raw).not.toContain("legacy-access-secret");
      const vault = new CredentialVault(workerEnv, `connectors/${state.id.toString()}`);
      const migrated = await vault.open<{
        version: number;
        connections: { google?: Record<string, unknown> };
      }>(row!.envelope);
      expect(migrated.value.version).toBe(2);
      expect(Object.keys(migrated.value.connections.google ?? {})).toEqual([
        connectors.gmail.connections[0]!.id,
      ]);
    });

    const removed = await SELF.fetch(
      `https://broker.test/users/${user}/connectors/google/connections/${connectors.gmail.connections[0]!.id}`,
      { method: "DELETE" },
    );
    expect(removed.status).toBe(204);
    const after = await connectorStatus(user);
    expect(after.gmail).toEqual({ connected: false, connections: [] });
    expect(after.gdrive).toEqual({ connected: false, connections: [] });
  });

  it("builds and validates Slack user OAuth responses", () => {
    const authorization = buildSlackAuthorizationUrl({
      clientId: "client", redirectUri, state: "state",
    });
    expect(authorization.searchParams.get("scope")).toBeNull();
    expect(authorization.searchParams.get("user_scope")).toBe(SLACK_PROVIDER.userScopes.join(","));
    expect(decodeSlackTokenResponse({
      ok: true,
      team: { id: "T123", name: "Workspace" },
      authed_user: {
        id: "U456", access_token: "xoxp-secret", token_type: "user",
        scope: SLACK_PROVIDER.userScopes.join(","),
      },
    })).toMatchObject({ teamId: "T123", userId: "U456" });
  });
});

type PublicConnection = {
  id: string;
  label: string;
  account_id: string;
  capabilities: string[];
};
type PublicStatus = Record<string, { connected: boolean; connections: PublicConnection[] }>;

async function connect(user: string, provider: "github" | "google" | "slack", code: string): Promise<string> {
  const started = await control(`/users/${user}/connectors/${provider}`, "POST", {
    redirect_uri: redirectUri,
    return_to: `/agent?provider=${provider}`,
  });
  expect(started.status).toBe(200);
  const state = new URL((await started.json<{ authorization_url: string }>()).authorization_url)
    .searchParams.get("state");
  const callback = await control(`/users/${user}/connectors/${provider}/callback`, "POST", {
    code, state,
  });
  expect(callback.status).toBe(200);
  return (await callback.json<{ connection_id: string }>()).connection_id;
}

async function connectorStatus(user: string): Promise<PublicStatus> {
  const response = await SELF.fetch(`https://broker.test/users/${user}/connectors`);
  expect(response.status).toBe(200);
  return (await response.json<{ connectors: PublicStatus }>()).connectors;
}

function control(path: string, method: string, body: unknown): Promise<Response> {
  return SELF.fetch(`https://broker.test${path}`, {
    method,
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}

async function bindSubject(subject: string, user: string): Promise<void> {
  const response = await control(`/subjects/${subject}`, "PUT", { user_id: user });
  expect(response.status).toBe(200);
}
