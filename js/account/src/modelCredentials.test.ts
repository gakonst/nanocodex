import assert from "node:assert/strict";
import test from "node:test";
import { decodeCredentialStatus } from "./modelCredentials.ts";

const base = { ready: true, active: "chatgpt", openai: { connected: false } };

test("account metadata preserves the full pool while another device login is pending", () => {
  const status = decodeCredentialStatus({ ...base, chatgpt: {
    connected: true, account_id: "first",
    accounts: [
      { account_id: "first", connected: true, active: true, access_token: "never expose" },
      { account_id: "second", connected: true, active: false, limited_until: 2000000000000 },
      { account_id: "expired", connected: false, active: false },
    ],
    login: { state: "pending", verification_url: "https://auth.openai.com/codex/device",
      user_code: "ABCD", expires_at: 2000000000000, poll_after_ms: 5000 },
  } });
  assert.deepEqual(status.chatgpt.accounts, [
    { accountId: "first", connected: true, active: true },
    { accountId: "second", connected: true, active: false, limitedUntil: 2000000000000 },
    { accountId: "expired", connected: false, active: false },
  ]);
  assert.equal(status.chatgpt.login?.userCode, "ABCD");
});

test("legacy single-account status remains visible and API-key preference is respected", () => {
  const status = decodeCredentialStatus({ ...base, active: "openai", chatgpt: { connected: true, account_id: "first" } });
  assert.deepEqual(status.chatgpt.accounts, [{ accountId: "first", connected: true, active: false }]);
  assert.deepEqual(decodeCredentialStatus({ ...base, chatgpt: { connected: false } }).chatgpt.accounts, []);
});

test("malformed account metadata fails instead of presenting misleading connection state", () => {
  for (const accounts of [null, {}, [{ account_id: "first" }], [
    { account_id: "first", connected: true, active: false, limited_until: "tomorrow" },
  ]]) {
    assert.throws(() => decodeCredentialStatus({ ...base, chatgpt: { connected: true, accounts } }), /Invalid ChatGPT account list/);
  }
});
