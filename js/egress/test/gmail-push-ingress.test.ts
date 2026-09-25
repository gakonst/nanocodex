import { describe, expect, it } from "vitest";
import { handleGmailPush, verifyGooglePushToken } from "../src/gmail-push-ingress";

// Boundary failures: forged/expired/wrong-audience or wrong-identity JWTs must
// never reach a mailbox; authenticated malformed payloads must not be acked.
// Verify real RSA signatures so a decoded-only JWT implementation cannot pass.
const config = { GMAIL_PUSH_OWNER_ID: "user", GMAIL_PUSH_CONNECTION_ID: "connection", GMAIL_PUSH_AUDIENCE: "https://app.example/v1/gmail-push", GMAIL_PUSH_SERVICE_ACCOUNT: "push@fixture.iam.gserviceaccount.com", GMAIL_PUSH_SUBSCRIPTION: "projects/fixture/subscriptions/gmail" };
const encode = (value: unknown) => btoa(JSON.stringify(value)).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
async function fixture(overrides: Record<string, unknown> = {}) {
  const keys = await crypto.subtle.generateKey({ name: "RSASSA-PKCS1-v1_5", modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: "SHA-256" }, true, ["sign", "verify"]) as CryptoKeyPair;
  const jwk = await crypto.subtle.exportKey("jwk", keys.publicKey);
  const now = Math.floor(Date.now() / 1000);
  const unsigned = `${encode({ alg: "RS256", kid: "fixture" })}.${encode({ iss: "https://accounts.google.com", aud: config.GMAIL_PUSH_AUDIENCE, email: config.GMAIL_PUSH_SERVICE_ACCOUNT, email_verified: true, sub: "123", iat: now, exp: now + 3600, ...overrides })}`;
  const signature = new Uint8Array(await crypto.subtle.sign("RSASSA-PKCS1-v1_5", keys.privateKey, new TextEncoder().encode(unsigned)));
  const token = `${unsigned}.${btoa(String.fromCharCode(...signature)).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "")}`;
  return { token, fetchKeys: async () => Response.json({ keys: [{ ...jwk, kid: "fixture", alg: "RS256", use: "sig" }] }) };
}
describe("Google Pub/Sub authentication", () => {
  it("accepts Google-signed configured identity and rejects forged signatures", async () => {
    const { token, fetchKeys } = await fixture();
    expect(await verifyGooglePushToken(token, config, fetchKeys)).toBe(true);
    expect(await verifyGooglePushToken(token.slice(0, -8) + "AAAAAAAA", config, fetchKeys)).toBe(false);
  });
  it("rejects valid signatures with invalid authority or lifetime", async () => {
    for (const invalid of [{ aud: "other" }, { email: "other@example.com" }, { email_verified: false }, { iss: "https://evil.example" }, { exp: 1 }, { iat: Math.floor(Date.now()/1000) + 600 }]) {
      const { token, fetchKeys } = await fixture(invalid);
      expect(await verifyGooglePushToken(token, config, fetchKeys)).toBe(false);
    }
  });
  it("rejects unconfigured or unauthenticated ingress without touching mailbox", async () => {
    const request = new Request("https://app.example/v1/gmail-push/user/connection", { method: "POST", body: "{}" });
    expect((await handleGmailPush(request, {})).status).toBe(503);
    expect((await handleGmailPush(request, config)).status).toBe(401);
  });
});
