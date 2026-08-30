import assert from "node:assert/strict";
import test from "node:test";

import {
  cliAppResource,
  cliOriginResource,
  approvedCliAccessKeyMatches,
  parseCliRegisterBody,
  sanitizeCliWalletResult,
  managedMemoryCapability,
  requestedConnectorsSatisfied,
} from "../src/devicePolicy.mjs";

const publicKey = "0x048318535b54105d4a7aae60c08fc45f9687181b4fdfc625bd1a753fa7397fed753547f11ca8696646f2f3acb08e31016afac23e630c5d11f59f61fef57b0d2aa5";
const keyAddress = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266";

function registration(resources, capabilities = {}) {
  const mpp = resources.includes("urn:nanocodex:mpp:machusd:spend");
  return {
    code_challenge: "challenge",
    code_challenge_method: "S256",
    message: {
      type: "rpc-requests",
      payload: [{
        jsonrpc: "2.0",
        id: 1,
        method: "wallet_connect",
        params: [{ capabilities: {
          authorizeAccessKey: {
            address: keyAddress,
            publicKey,
            keyType: "secp256k1",
            chainId: "0x1079",
            expiry: Math.floor(Date.now() / 1_000) + 30 * 86_400,
            limits: mpp ? [
              { token: "0x20c0000000000000000000006637932dE5413804", limit: "0x989680", period: 86_400 },
              { token: "0x20C000000000000000000000b9537d11c60E8b50", limit: "0x989680", period: 86_400 },
            ] : [
              { token: "0x20c0000000000000000000006637932dE5413804", limit: "0x0", period: 0 },
              { token: "0x20C000000000000000000000b9537d11c60E8b50", limit: "0x0", period: 0 },
            ],
            scopes: mpp ? [
              { address: "0x20C000000000000000000000b9537d11c60E8b50", selector: "0xa9059cbb", recipients: ["0xa295C42FBCC026a62304A7701f25B4c91799B0dA"] },
              { address: "0x20C000000000000000000000b9537d11c60E8b50", selector: "0x95777d59", recipients: ["0xa295C42FBCC026a62304A7701f25B4c91799B0dA"] },
              { address: "0x20c0000000000000000000006637932dE5413804", selector: "0x095ea7b3", recipients: ["0xd588ED9Ae08643A450157Adaf61c3C0C1BBd0dbb"] },
              { address: "0xd588ED9Ae08643A450157Adaf61c3C0C1BBd0dbb", selector: "0x34189fed" },
              { address: "0x4d50500000000000000000000000000000000000", selector: "0xedc53b00" },
              { address: "0x4d50500000000000000000000000000000000000", selector: "0xdc48471e" },
            ] : [],
          },
          ...capabilities,
          auth: { resources },
        } }],
      }],
    },
  };
}

const base = ["urn:nanocodex:agent:run", cliAppResource, cliOriginResource];
const credentialImport =
  `urn:nanocodex:credential-import:chatgpt:codex-auth-v1:sha256:${"a".repeat(43)}`;

test("CLI device registration accepts exact hosted capabilities without implicit MPP", () => {
  const resources = [
    ...base,
    "urn:nanocodex:agent:output:final",
    "urn:nanocodex:history:read",
    "urn:nanocodex:memory:read",
    "urn:nanocodex:memory:write",
    "urn:nanocodex:connectors:chatgpt,github",
  ];
  assert.deepEqual(parseCliRegisterBody(registration(resources)).resources, resources);
});

test("CLI connector focus is signed, singular, and part of the granted connector set", () => {
  const whoop = [
    ...base,
    "urn:nanocodex:connectors:whoop",
    "urn:nanocodex:connector-focus:whoop",
  ];
  assert.deepEqual(parseCliRegisterBody(registration(whoop)).resources, whoop);
  const focused = [
    ...base,
    "urn:nanocodex:connectors:chatgpt,github",
    "urn:nanocodex:connector-focus:github",
  ];
  assert.deepEqual(parseCliRegisterBody(registration(focused)).resources, focused);
  assert.throws(() => parseCliRegisterBody(registration([
    ...base,
    "urn:nanocodex:connectors:chatgpt",
    "urn:nanocodex:connector-focus:github",
  ])), /focus/);
  assert.throws(() => parseCliRegisterBody(registration([
    ...base,
    "urn:nanocodex:connectors:chatgpt,github",
    "urn:nanocodex:connector-focus:chatgpt",
    "urn:nanocodex:connector-focus:github",
  ])), /focus/);
});

test("CLI credential import is singular, well formed, and bound to ChatGPT", () => {
  assert.doesNotThrow(() => parseCliRegisterBody(registration([
    ...base,
    "urn:nanocodex:connectors:chatgpt",
    credentialImport,
  ])));
  assert.throws(() => parseCliRegisterBody(registration([...base, credentialImport])), /ChatGPT/);
  assert.throws(() => parseCliRegisterBody(registration([
    ...base,
    "urn:nanocodex:connectors:chatgpt",
    credentialImport,
    credentialImport,
  ])), /resources/);
  assert.throws(() => parseCliRegisterBody(registration([
    ...base,
    "urn:nanocodex:connectors:chatgpt",
    `${credentialImport}x`,
  ])), /resources/);
  assert.throws(() => parseCliRegisterBody(registration([
    "urn:nanocodex:agent:run",
    "urn:nanocodex:app:attacker",
    cliOriginResource,
    "urn:nanocodex:connectors:chatgpt",
    credentialImport,
  ])), /resources/);
});

test("CLI remote MCP focus is signed, singular, and part of the exact connection set", () => {
  const id = "a".repeat(43);
  const other = "b".repeat(43);
  assert.doesNotThrow(() => parseCliRegisterBody(registration([
    ...base,
    `urn:nanocodex:mcp:${id}`,
    `urn:nanocodex:mcp-focus:${id}`,
  ])));
  assert.throws(() => parseCliRegisterBody(registration([
    ...base,
    `urn:nanocodex:mcp:${id}`,
    `urn:nanocodex:mcp-focus:${other}`,
  ])), /MCP connection resources/);
});

test("CLI device registration always binds a prepared installation access key", () => {
  const resources = [
    ...base,
    "urn:nanocodex:capability:mercator:boost",
    "urn:nanocodex:mpp:machusd:spend",
  ];
  assert.deepEqual(parseCliRegisterBody(registration(resources)).resources, resources);
  assert.throws(() => parseCliRegisterBody(registration(resources, {
    authorizeAccessKey: undefined,
  })), /access-key/);
  const missingLimits = registration(base);
  delete missingLimits.message.payload[0].params[0].capabilities.authorizeAccessKey.limits;
  assert.throws(() => parseCliRegisterBody(missingLimits), /policy/);
  const unrequestedSpend = registration(base);
  unrequestedSpend.message.payload[0].params[0].capabilities.authorizeAccessKey.limits = [{
    token: "0x20c0000000000000000000006637932dE5413804",
    limit: "0x989680",
    period: 86_400,
  }];
  assert.throws(() => parseCliRegisterBody(unrequestedSpend), /without MPP/);
});

test("CLI device registration rejects expanded, administrative, stale, and wrong-chain key policies", () => {
  const future = Math.floor(Date.now() / 1_000) + 30 * 86_400;
  assert.throws(() => parseCliRegisterBody(registration(base, {
    authorizeAccessKey: {
      address: keyAddress,
      publicKey,
      keyType: "secp256k1",
      chainId: "0x1",
      expiry: future,
      limits: [],
      scopes: [],
    },
  })), /policy/);
  const withAdmin = registration(base);
  withAdmin.message.payload[0].params[0].capabilities.authorizeAccessKey.isAdmin = true;
  assert.throws(() => parseCliRegisterBody(withAdmin), /policy/);
  const stale = registration(base);
  stale.message.payload[0].params[0].capabilities.authorizeAccessKey.expiry = 1;
  assert.throws(() => parseCliRegisterBody(stale), /policy/);
  const expanded = registration([
    ...base,
    "urn:nanocodex:capability:mercator:boost",
    "urn:nanocodex:mpp:machusd:spend",
  ]);
  expanded.message.payload[0].params[0].capabilities.authorizeAccessKey.limits[0].limit = "0x989681";
  assert.throws(() => parseCliRegisterBody(expanded), /MPP/);
});

test("signed CLI access keys must exactly match the retained installation policy", () => {
  const pending = parseCliRegisterBody(registration(base));
  const request = pending.params[0].capabilities.authorizeAccessKey;
  const approved = {
    address: keyAddress,
    key_id: keyAddress,
    key_type: "secp256k1",
    chain_id: "4217",
    expiry: request.expiry,
    limits: request.limits.map((limit) => ({
      token: limit.token.toLowerCase(),
      limit: "0",
      period: limit.period,
    })),
    scopes: [],
  };
  assert.equal(approvedCliAccessKeyMatches(pending, approved), true);
  assert.equal(approvedCliAccessKeyMatches(pending, { ...approved, chain_id: "1" }), false);
  assert.equal(approvedCliAccessKeyMatches(pending, { ...approved, expiry: request.expiry + 1 }), false);
  assert.equal(approvedCliAccessKeyMatches(pending, {
    ...approved,
    scopes: [{ address: keyAddress, selector: "0xa9059cbb" }],
  }), false);
});

test("CLI device registration rejects wrong identities, methods, extras, and duplicates", () => {
  assert.throws(() => parseCliRegisterBody(registration([
    "urn:nanocodex:agent:run",
    "urn:nanocodex:app:attacker",
    cliOriginResource,
  ])), /resources/);
  assert.throws(() => parseCliRegisterBody(registration([...base, "urn:nanocodex:admin"])), /resources/);
  assert.throws(() => parseCliRegisterBody(registration([...base, cliAppResource])), /resources/);
  const wrongMethod = registration(base);
  wrongMethod.message.payload[0].method = "personal_sign";
  assert.throws(() => parseCliRegisterBody(wrongMethod), /wallet_connect/);
});

test("CLI result sanitizer permits only the signed key and one-use approval id", () => {
  const keyAuthorization = { keyId: "0x0000000000000000000000000000000000000002" };
  const result = sanitizeCliWalletResult({
    accounts: [{
      address: "0x0000000000000000000000000000000000000001",
      capabilities: {
        auth: { approval_id: "a".repeat(43) },
        keyAuthorization,
        personalSign: { keyAuthorization: "0x1234" },
      },
    }],
  });
  assert.strictEqual(result.accounts[0].capabilities.keyAuthorization, keyAuthorization);
  assert.deepEqual(result.accounts[0].capabilities.personalSign, { keyAuthorization: "0x1234" });
  assert.deepEqual(result.accounts[0].capabilities.auth, { approval_id: "a".repeat(43) });
  assert.throws(() => sanitizeCliWalletResult({
    accounts: [{
      address: "0x0000000000000000000000000000000000000001",
      capabilities: {
        auth: { approval_id: "a".repeat(43), token: "secret" },
        keyAuthorization,
        personalSign: { keyAuthorization: "0x1234" },
      },
    }],
  }), /invalid CLI approval/);
});

test("CLI result sanitizer accepts a hosted approval only without key material", () => {
  const hosted = sanitizeCliWalletResult({
    accounts: [{
      address: "0x0000000000000000000000000000000000000001",
      capabilities: { auth: { approval_id: "h".repeat(43), mode: "hosted" } },
    }],
  });
  assert.deepEqual(hosted.accounts[0].capabilities, {
    auth: { approval_id: "h".repeat(43), mode: "hosted" },
  });
  assert.throws(() => sanitizeCliWalletResult({
    accounts: [{
      address: "0x0000000000000000000000000000000000000001",
      capabilities: {
        auth: { approval_id: "h".repeat(43), mode: "hosted" },
        personalSign: { keyAuthorization: "0x1234" },
      },
    }],
  }), /invalid hosted CLI approval/);
});

test("hosted history and memory paths map to narrow grant capabilities", () => {
  assert.strictEqual(
    managedMemoryCapability("/v1/history/sessions/search"),
    "history:read",
  );
  assert.strictEqual(
    managedMemoryCapability("/v1/history/sessions/session-1/read"),
    "history:read",
  );
  assert.strictEqual(managedMemoryCapability("/v1/memory", "scan"), "memory:read");
  assert.strictEqual(managedMemoryCapability("/v1/memory", "read"), "memory:read");
  assert.strictEqual(managedMemoryCapability("/v1/memory", "put"), "memory:write");
  assert.strictEqual(managedMemoryCapability("/v1/memory", "delete"), "memory:write");
  assert.strictEqual(managedMemoryCapability("/v1/memory", "admin"), undefined);
  assert.strictEqual(managedMemoryCapability("/v1/agents/other", "read"), undefined);
});

test("connector grants require an exact live requested set", () => {
  assert.strictEqual(requestedConnectorsSatisfied(["chatgpt", "github"], ["chatgpt", "github"]), true);
  assert.strictEqual(requestedConnectorsSatisfied(["chatgpt"], ["chatgpt", "github"]), false);
  assert.strictEqual(requestedConnectorsSatisfied(["chatgpt", "github", "x"], ["chatgpt", "github"]), false);
});
