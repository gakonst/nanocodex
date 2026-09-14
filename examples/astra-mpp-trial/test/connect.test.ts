import assert from "node:assert/strict";
import test from "node:test";
import {
  ConnectVerificationError,
  connectConfiguration,
  connectCredentials,
  connectIdentityFromWire,
  verifyConnectIdentity,
} from "../src/connect.ts";

const grantId = `0x${"ab".repeat(32)}` as const;
const token = "t".repeat(43);
const accountId = "d9428888-122b-4c26-96af-2d8cf03b2e33";
const accountAddress = "0x1234567890abcdef1234567890abcdef12345678";
const configuration = {
  apiUrl: "https://connect.example/",
  appId: "astra-demo",
  appOrigin: "https://astra.example",
  dialogUrl: "https://dialog.example/",
};

function wire(overrides: Record<string, unknown> = {}) {
  return {
    account_id: accountId,
    account_address: accountAddress,
    authorization_mode: "access_key",
    grant: {
      id: grantId,
      permission: "agent.run",
      status: "active",
      expires_at: Math.floor(Date.now() / 1_000) + 3_600,
      capabilities: ["nanocodex.agent", "mpp.mach"],
    },
    ...overrides,
  };
}

test("accepts only exact public Connect configuration", () => {
  assert.deepEqual(connectConfiguration({
    NANOCODEX_CONNECT_API_URL: configuration.apiUrl,
    NANOCODEX_CONNECT_APP_ID: configuration.appId,
    NANOCODEX_CONNECT_APP_ORIGIN: configuration.appOrigin,
    NANOCODEX_CONNECT_DIALOG_URL: configuration.dialogUrl,
  }), configuration);
  assert.equal(connectConfiguration({
    NANOCODEX_CONNECT_API_URL: configuration.apiUrl,
    NANOCODEX_CONNECT_APP_ID: configuration.appId,
    NANOCODEX_CONNECT_APP_ORIGIN: "https://astra.example/path",
    NANOCODEX_CONNECT_DIALOG_URL: configuration.dialogUrl,
  }), undefined);
  assert.equal(connectConfiguration({
    NANOCODEX_CONNECT_API_URL: configuration.apiUrl,
    NANOCODEX_CONNECT_APP_ID: configuration.appId,
    NANOCODEX_CONNECT_APP_ORIGIN: "http://astra.nanocodex.localhost:8787",
    NANOCODEX_CONNECT_DIALOG_URL: configuration.dialogUrl,
  })?.appOrigin, "http://astra.nanocodex.localhost:8787");
  assert.equal(connectConfiguration({
    NANOCODEX_CONNECT_API_URL: configuration.apiUrl,
    NANOCODEX_CONNECT_APP_ID: configuration.appId,
    NANOCODEX_CONNECT_APP_ORIGIN: "http://astra.example",
    NANOCODEX_CONNECT_DIALOG_URL: configuration.dialogUrl,
  }), undefined);
});

test("reads the grant token only from bearer auth and the grant ID header", () => {
  const request = new Request("https://astra.example/api/session", {
    headers: {
      authorization: `Bearer ${token}`,
      "x-nanocodex-grant-id": grantId,
    },
  });
  assert.deepEqual(connectCredentials(request), { grantId, token });
  assert.equal(connectCredentials(new Request(request.url)), undefined);
});

test("projects only a live account access-key grant", () => {
  assert.deepEqual(connectIdentityFromWire(wire(), grantId), {
    accountAddress,
    accountId,
    expiresAt: (wire().grant as { expires_at: number }).expires_at,
    grantId,
  });
  assert.throws(
    () => connectIdentityFromWire(wire({ authorization_mode: "hosted" }), grantId),
    ConnectVerificationError,
  );
  assert.throws(
    () => connectIdentityFromWire(wire({ grant: { ...(wire().grant as object), status: "revoked" } }), grantId),
    ConnectVerificationError,
  );
});

test("introspects through the exact Connect app and never forwards browser cookies", async () => {
  let observed: Request | undefined;
  const identity = await verifyConnectIdentity({ grantId, token }, configuration, async (input, init) => {
    observed = new Request(input, init);
    return Response.json(wire());
  });
  assert.equal(identity.accountId, accountId);
  assert.equal(observed?.url, `https://connect.example/v1/grants/${grantId}`);
  assert.equal(observed?.headers.get("authorization"), `Bearer ${token}`);
  assert.equal(observed?.headers.get("origin"), configuration.appOrigin);
  assert.equal(observed?.headers.get("x-nanocodex-app-id"), configuration.appId);
  assert.equal(observed?.headers.get("cookie"), null);
});

test("fails closed when Connect rejects or cannot resolve the grant", async () => {
  await assert.rejects(
    verifyConnectIdentity({ grantId, token }, configuration, async () => Response.json({}, { status: 401 })),
    (error: unknown) => error instanceof ConnectVerificationError && error.status === 401,
  );
  await assert.rejects(
    verifyConnectIdentity({ grantId, token }, configuration, async () => { throw new Error("offline"); }),
    (error: unknown) => error instanceof ConnectVerificationError && error.status === 503,
  );
});
