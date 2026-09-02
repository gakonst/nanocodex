import { env } from "cloudflare:workers";
import { runDurableObjectAlarm, SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";

import { buildGDriveAuthorizationParams } from "../src/connectors/gdrive";
import { buildGmailAuthorizationParams } from "../src/connectors/gmail";
import type { EgressEnv } from "../src/egress";

const authorizationInput = {
  clientId: "google-client-id",
  redirectUri: "https://nanocodex.test/v1/connectors/gmail/callback",
  state: "state",
  codeChallenge: "A".repeat(43),
};
const workerEnv = env as unknown as EgressEnv;

describe("Google connector account hints", () => {
  it("passes an exact login hint to Gmail and Google Drive", () => {
    expect(Object.fromEntries(buildGmailAuthorizationParams({
      ...authorizationInput,
      loginHint: "reader@example.com",
    }))).toMatchObject({ login_hint: "reader@example.com" });
    expect(Object.fromEntries(buildGDriveAuthorizationParams({
      ...authorizationInput,
      loginHint: "reader@example.com",
    }))).toMatchObject({ login_hint: "reader@example.com" });
  });

  it("targets and verifies the exact Google account requested by the agent", async () => {
    const user = "connector-agent-email";
    const started = await control(`/users/${user}/connectors/gmail`, "POST", {
      redirect_uri: authorizationInput.redirectUri,
      return_to: "/agent?thread=connector-agent-email",
      account_hint: "mail@example.test",
    });
    expect(started.status).toBe(200);
    const authorization = new URL(
      (await started.json<{ authorization_url: string }>()).authorization_url,
    );
    expect(authorization.searchParams.get("login_hint")).toBe("mail@example.test");

    const completed = await control(`/users/${user}/connectors/gmail/callback`, "POST", {
      code: "gmail-code",
      state: authorization.searchParams.get("state"),
    });
    expect(completed.status).toBe(200);
    expect(await completed.json()).toMatchObject({
      connected: true,
      return_to: "/agent?thread=connector-agent-email",
    });
    expect(await (await SELF.fetch(`https://broker.test/users/${user}/connectors`)).json())
      .toMatchObject({ connectors: { gmail: {
        connected: true,
        connections: [{ label: "mail@example.test", capabilities: ["gmail"] }],
      } } });
  });

  it("rejects and revokes a Google grant for a different account", async () => {
    const user = "connector-agent-email-mismatch";
    const started = await control(`/users/${user}/connectors/gmail`, "POST", {
      redirect_uri: authorizationInput.redirectUri,
      return_to: "/agent?thread=connector-agent-email-mismatch",
      account_hint: "other@example.test",
    });
    const authorization = new URL(
      (await started.json<{ authorization_url: string }>()).authorization_url,
    );

    const completed = await control(`/users/${user}/connectors/gmail/callback`, "POST", {
      code: "gmail-code",
      state: authorization.searchParams.get("state"),
    });
    expect(completed.status).toBe(409);
    expect(await completed.json()).toEqual({
      error: "connector_account_mismatch",
      return_to: "/agent?thread=connector-agent-email-mismatch",
    });
    expect(await (await SELF.fetch(`https://broker.test/users/${user}/connectors`)).json())
      .toMatchObject({ connectors: { gmail: { connected: false } } });
  });

  it("durably retries a mismatched grant revocation after a provider failure", async () => {
    const user = "connector-agent-email-revoke-retry";
    const started = await control(`/users/${user}/connectors/gmail`, "POST", {
      redirect_uri: authorizationInput.redirectUri,
      return_to: "/agent?thread=connector-agent-email-revoke-retry",
      account_hint: "other@example.test",
    });
    const authorization = new URL(
      (await started.json<{ authorization_url: string }>()).authorization_url,
    );
    const completed = await control(`/users/${user}/connectors/gmail/callback`, "POST", {
      code: "gmail-revoke-once-code",
      state: authorization.searchParams.get("state"),
    });
    expect(completed.status).toBe(409);
    expect(await (await SELF.fetch(`https://broker.test/users/${user}/connectors`)).json())
      .toMatchObject({ connectors: { gmail: { connected: false } } });

    const broker = workerEnv.USER_CONNECTORS.getByName(user);
    expect(await runDurableObjectAlarm(broker)).toBe(true);
    expect(await runDurableObjectAlarm(broker)).toBe(false);
    expect(await (await SELF.fetch(`https://broker.test/users/${user}/connectors`)).json())
      .toMatchObject({ connectors: { gmail: { connected: false } } });
  });
});

function control(path: string, method: string, body: unknown): Promise<Response> {
  return SELF.fetch(`https://broker.test${path}`, {
    method,
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}
