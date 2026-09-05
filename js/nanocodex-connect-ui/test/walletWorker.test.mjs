import assert from "node:assert/strict";
import { test } from "node:test";

import {
  requestManagedWalletConnect,
  requestManagedWalletRevocation,
} from "../dist/walletWorker.mjs";

const address = "0x1234567890123456789012345678901234567890";
const approval = "a".repeat(43);

test("the SMS wallet helper posts a same-origin request and returns only the sanitized result", async () => {
  let call;
  const connected = await requestManagedWalletConnect({ method: "wallet_connect" }, true, async (url, init) => {
    call = { url, init };
    return {
      ok: true,
      json: async () => ({
        accounts: [{
            address,
            capabilities: {
              auth: { approval_id: approval, token: "browser-only-token" },
              keyAuthorization: { signed: true },
              personalSign: { keyAuthorization: "0x12" },
            },
        }],
      }),
    };
  });
  assert.deepEqual(call, {
    url: "/v1/wallet/connect",
    init: {
      body: JSON.stringify({ request: { method: "wallet_connect" } }),
      credentials: "include",
      headers: { "content-type": "application/json" },
      method: "POST",
    },
  });
  assert.equal(connected.authToken, "browser-only-token");
  assert.deepEqual(connected.result, {
    accounts: [{
      address,
      capabilities: {
        auth: { approval_id: approval },
        keyAuthorization: { signed: true },
        personalSign: { keyAuthorization: "0x12" },
      },
    }],
  });
});

test("the Worker helper rejects a private key and revokes through the same-origin Worker", async () => {
  await assert.rejects(
    requestManagedWalletConnect({ method: "wallet_connect" }, false, async () => ({
      ok: true,
      json: async () => ({ private_key: "never" }),
    })),
    /private key material/,
  );
  let call;
  const request = {
    method: "wallet_revokeAccessKey",
    params: [{ address, accessKeyAddress: "0x2222222222222222222222222222222222222222" }],
  };
  const result = await requestManagedWalletRevocation(request, address, async (url, init) => {
    call = { url, init };
    return { ok: true, json: async () => ({ ok: true }) };
  });
  assert.equal(result, undefined);
  assert.deepEqual(call, {
    url: "/v1/wallet/revoke-access-key",
    init: {
      body: JSON.stringify({ request }),
      credentials: "include",
      headers: { "content-type": "application/json" },
      method: "POST",
    },
  });

  await assert.rejects(
    requestManagedWalletRevocation(
      request,
      "0x3333333333333333333333333333333333333333",
      async () => { throw new Error("must not fetch"); },
    ),
    /owns this access key/,
  );
});
