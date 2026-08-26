import assert from "node:assert/strict";
import test from "node:test";

import {
  accountLoginCapabilities,
  appVisibilityPermissions,
  connectApiOrigin,
  isLocalDevelopmentOrigin,
  isPopupPresentation,
  productionConnectApiOrigin,
  registeredApp,
  sanitizeWalletResult,
  signedAppResources,
  usesBrowserLocalWebAuthn,
} from "../src/connectPolicy.mjs";

test("local development origins use only the canonical localhost domain", () => {
  assert.equal(isLocalDevelopmentOrigin("http://nanocodex.localhost:5173"), true);
  assert.equal(isLocalDevelopmentOrigin("http://passkey-a.nanocodex.localhost:5173"), true);
  assert.equal(isLocalDevelopmentOrigin("http://playground-passkey-a.nanocodex.localhost:5173"), true);
  assert.equal(isLocalDevelopmentOrigin("http://nanocodex.example:5173"), false);
  assert.equal(isLocalDevelopmentOrigin("https://connect.example"), false);
});

test("only standalone loopback dialogs use the browser-local WebAuthn ceremony", () => {
  assert.equal(usesBrowserLocalWebAuthn("http://127.0.0.1:4177"), true);
  assert.equal(usesBrowserLocalWebAuthn("https://localhost:4177"), true);
  assert.equal(usesBrowserLocalWebAuthn("http://nanocodex.localhost:4177"), false);
  assert.equal(usesBrowserLocalWebAuthn("https://nanocodex.example"), false);
  assert.equal(usesBrowserLocalWebAuthn("https://nanocodex.gakonst.workers.dev"), false);
});

const playground = "https://nanocodex-connect-playground.gakonst.workers.dev";
const chromeExtension = "chrome-extension://jpkimkgbgbpcaldbnhlhbkbadmpeffle";
const productionDialog = "https://nanocodex.gakonst.workers.dev/connect-dialog/?mode=iframe";

test("existing-account login targets only credentials retained by this dialog", () => {
  assert.deepEqual(accountLoginCapabilities([
    { credential: { id: "known-passkey" } },
    { credential: { id: "known-passkey" } },
    { credential: { id: "second-passkey" } },
  ]), {
    method: "login",
    credentialId: ["known-passkey", "second-passkey"],
  });
  assert.deepEqual(accountLoginCapabilities([]), {
    method: "login",
  });
});

test("signed agent visibility resources map to compact consent labels", () => {
  assert.deepEqual(appVisibilityPermissions([
    "urn:nanocodex:agent:visibility:reply,actions,history,traces",
  ]), [
    {
      resource: "urn:nanocodex:agent:output:final",
      label: "Reply",
      detail: "Final agent reply",
    },
    {
      resource: "urn:nanocodex:agent:output:actions",
      label: "Actions",
      detail: "Agent actions and tool calls",
    },
    {
      resource: "urn:nanocodex:agent:history:read",
      label: "History",
      detail: "Conversation history",
    },
    {
      resource: "urn:nanocodex:agent:trace:read",
      label: "Traces",
      detail: "Full run trace",
    },
  ]);
});

test("unsigned and malformed resources do not produce visibility claims", () => {
  assert.deepEqual(appVisibilityPermissions([
    "urn:nanocodex:agent:output",
    "urn:nanocodex:agent:trace:write",
    null,
  ]), []);
  assert.deepEqual(appVisibilityPermissions(undefined), []);
});

test("legacy visibility resources remain readable", () => {
  assert.deepEqual(appVisibilityPermissions([
    "urn:nanocodex:agent:output:final",
    "urn:nanocodex:agent:trace:read",
  ]).map(({ label }) => label), ["Reply", "Traces"]);
});

test("production Connect policy pins the API and registered embedding app", () => {
  assert.equal(connectApiOrigin({
    challenge: `${productionConnectApiOrigin}/v1/connect/auth/challenge`,
    url: `${productionConnectApiOrigin}/v1/connect/auth`,
  }, "https://nanocodex-connect.gakonst.workers.dev"), productionConnectApiOrigin);
  assert.deepEqual(registeredApp(playground, "atlas-workspace", productionDialog, false), {
    id: "atlas-workspace",
    name: "Atlas Workspace",
    origin: playground,
  });
  assert.deepEqual(registeredApp(chromeExtension, "nanocodex-chrome", productionDialog, false), {
    id: "nanocodex-chrome",
    name: "Nanocodex for Chrome",
    origin: chromeExtension,
  });
});

test("only top-level popup dialogs admit unknown secure app origins", () => {
  assert.throws(() => connectApiOrigin({
    challenge: `${productionConnectApiOrigin}/v1/connect/auth/challenge`,
    verify: `${productionConnectApiOrigin}/v1/connect/auth`,
    logout: "https://attacker.example/collect",
  }, "https://nanocodex-connect.gakonst.workers.dev"), /production Connect API/);
  assert.deepEqual(registeredApp(
    "https://consumer.example",
    "consumer-example",
    "https://nanocodex.gakonst.workers.dev/connect-dialog/?mode=popup",
    true,
  ), {
    id: "consumer-example",
    name: "consumer.example",
    origin: "https://consumer.example",
  });
  assert.deepEqual(registeredApp(
    "chrome-extension://abcdefghijklmnopabcdefghijklmnop",
    "consumer-extension",
    "https://nanocodex.gakonst.workers.dev/connect-dialog/?mode=popup",
    true,
  ).origin, "chrome-extension://abcdefghijklmnopabcdefghijklmnop");
  assert.throws(() => registeredApp(
    "https://consumer.example",
    "consumer-example",
    "https://nanocodex.gakonst.workers.dev/connect-dialog/?mode=popup",
    false,
  ), /not registered/);
  assert.throws(() => registeredApp(
    "http://attacker.example",
    "attacker",
    "https://nanocodex.gakonst.workers.dev/connect-dialog/?mode=popup",
    true,
  ), /not registered/);
  assert.equal(isPopupPresentation("https://dialog.example/?mode=popup", true), true);
  assert.equal(isPopupPresentation("https://dialog.example/?mode=popup", false), false);
  assert.equal(isPopupPresentation("https://dialog.example/?mode=iframe", true), false);
});

test("loopback auth and apps are accepted only by a loopback dialog", () => {
  assert.equal(connectApiOrigin({
    challenge: `${productionConnectApiOrigin}/v1/connect/auth/challenge`,
    verify: `${productionConnectApiOrigin}/v1/connect/auth`,
  }, "http://127.0.0.1:4177"), productionConnectApiOrigin);
  assert.equal(connectApiOrigin({
    challenge: "http://127.0.0.1:8787/v1/connect/auth/challenge",
    verify: "http://127.0.0.1:8787/v1/connect/auth",
  }, "http://127.0.0.1:4177"), "http://127.0.0.1:8787");
  assert.equal(registeredApp(
    "http://localhost:4173",
    "atlas-workspace",
    "http://127.0.0.1:4177/connect-dialog/?mode=iframe",
    false,
  ).id, "atlas-workspace");
  assert.throws(() => connectApiOrigin({ url: "http://127.0.0.1:8787/v1/connect/auth" }, "https://dialog.example"), /production Connect API/);
  assert.throws(() => connectApiOrigin({
    challenge: "http://127.0.0.1:8787/v1/connect/auth/challenge",
    verify: "http://localhost:8787/v1/connect/auth",
  }, "http://127.0.0.1:4177"), /share one development origin/);
});

test("signed app and origin resources bind the dialog app exactly once", () => {
  const app = registeredApp(
    "https://consumer.example",
    "consumer-example",
    "https://nanocodex.gakonst.workers.dev/connect-dialog/?mode=popup",
    true,
  );
  const resources = [
    "urn:nanocodex:agent:run",
    "urn:nanocodex:app:consumer-example",
    "urn:nanocodex:origin:https%3A%2F%2Fconsumer.example",
  ];
  assert.strictEqual(signedAppResources(resources, app), resources);
  assert.throws(() => signedAppResources([
    "urn:nanocodex:app:other-app",
    "urn:nanocodex:origin:https%3A%2F%2Fconsumer.example",
  ], app), /do not match/);
  assert.throws(() => signedAppResources([
    "urn:nanocodex:app:consumer-example",
    "urn:nanocodex:origin:https%3A%2F%2Fother.example",
  ], app), /do not match/);
});

test("wallet result sanitization retains signatures without exposing the account bearer", () => {
  const keyAuthorization = { address: "0xkey", witness: "0xwitness" };
  const personalSign = { keyAuthorization: "0xsigned", message: "0xmessage" };
  const result = sanitizeWalletResult({
    accounts: [{
      address: "0x0000000000000000000000000000000000000001",
      capabilities: {
        auth: { approval_id: "approval-1", token: "account-wide-secret", agent_id: "agent-1" },
        keyAuthorization,
        personalSign,
      },
    }],
  });
  assert.deepEqual(result.accounts[0].capabilities.auth, { approval_id: "approval-1" });
  assert.strictEqual(result.accounts[0].capabilities.keyAuthorization, keyAuthorization);
  assert.strictEqual(result.accounts[0].capabilities.personalSign, personalSign);
  assert.equal(JSON.stringify(result).includes("account-wide-secret"), false);
});
