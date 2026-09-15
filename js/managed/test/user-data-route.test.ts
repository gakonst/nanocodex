import { env } from "cloudflare:test";
import { describe, expect, it } from "vitest";

import type { Principal } from "../src/account-auth";
import { userDataTool } from "../src/user-data-tool";
import { routeUserDataRequest, type UserDataRouteEnv } from "../src/user-data-route";

const USER_A = "11111111-1111-4111-8111-111111111111";
const USER_B = "22222222-2222-4222-8222-222222222222";
const USER_C = "33333333-3333-4333-8333-333333333333";

describe("per-user data HTTP journey", () => {
  it("ingests a WHOOP batch, reads it back, aggregates it, and isolates users", async () => {
    const asUserA = principal(USER_A, ["data:read", "data:write"]);
    const asUserB = principal(USER_B, ["data:read", "data:write"]);

    await expect(call(asUserA, {
      operation: "object_put",
      key: "whoop/raw/batch-1.json.gz",
      content: "H4sIAAAAAAAA/6tWykjNyclXslIqzy/KSVEEAB0JBF4PAAAA",
      encoding: "base64",
      content_type: "application/gzip",
      metadata: { integration: "whoop", immutable: true },
    })).resolves.toMatchObject({ status: 200, body: { object: { version: 1 } } });

    const write = {
      operation: "timeseries_write",
      series: "whoop.recovery_score",
      points: [
        { timestamp_ms: 1_000, value: 61, fields: { cycle_id: "cycle-1" } },
        { timestamp_ms: 2_000, value: 79, fields: { cycle_id: "cycle-2" } },
      ],
    };
    await expect(call(asUserA, write)).resolves.toMatchObject({
      status: 200,
      body: { inserted: 2, replayed: 0 },
    });
    await expect(call(asUserA, write)).resolves.toMatchObject({
      status: 200,
      body: { inserted: 0, replayed: 2 },
    });
    await expect(call(asUserA, {
      operation: "timeseries_aggregate",
      series: "whoop.recovery_score",
      start_ms: 1_000,
      end_ms: 2_000,
      bucket_ms: 2_000,
      aggregation: "avg",
    })).resolves.toMatchObject({
      status: 200,
      body: { buckets: [{ start_ms: 1_000, value: 70, count: 2 }] },
    });
    await expect(call(asUserA, {
      operation: "object_get",
      key: "whoop/raw/batch-1.json.gz",
      encoding: "base64",
    })).resolves.toMatchObject({
      status: 200,
      body: { object: { content: "H4sIAAAAAAAA/6tWykjNyclXslIqzy/KSVEEAB0JBF4PAAAA" } },
    });

    await expect(call(asUserB, {
      operation: "timeseries_query",
      series: "whoop.recovery_score",
    })).resolves.toMatchObject({ status: 200, body: { points: [] } });
    await expect(call(asUserB, {
      operation: "object_get",
      key: "whoop/raw/batch-1.json.gz",
    })).resolves.toMatchObject({ status: 404, body: { error: "not_found" } });
  });

  it("enforces read/write capabilities at the public boundary", async () => {
    await expect(call(principal(USER_A, ["data:read"]), {
      operation: "timeseries_write",
      series: "whoop.strain",
      points: [{ timestamp_ms: 1, value: 12.5 }],
    })).resolves.toMatchObject({ status: 403, body: { error: "forbidden" } });
    await expect(call(undefined, { operation: "document_list" }))
      .resolves.toMatchObject({ status: 401, body: { error: "unauthorized" } });
  });

  it("exposes the identical contract through the user_data agent tool", async () => {
    const authenticated = principal(USER_C, ["data:read", "data:write"]);
    const tool = userDataTool({
      requireCapability: (capability) => {
        if (!authenticated.capabilities.includes(capability)) throw new Error("forbidden");
      },
      execute: async (operation) => {
        const result = await call(authenticated, operation);
        if (result.status !== 200) throw new Error(`user data returned HTTP ${result.status}`);
        return result.body;
      },
    });
    const context = {
      callId: "call-e2e",
      parentCallId: "root",
      sessionId: crypto.randomUUID(),
      model: "test",
      signal: new AbortController().signal,
    };

    await expect(tool.handler({
      operation: "document_put",
      key: "whoop/profile",
      value: { user_id: "whoop-fixture", max_hr: 190 },
    }, context)).resolves.toMatchObject({ document: { version: 1 } });
    await expect(tool.handler({
      operation: "document_get",
      key: "whoop/profile",
    }, context)).resolves.toMatchObject({
      document: { value: { user_id: "whoop-fixture", max_hr: 190 } },
    });
  });
});

function principal(userId: string, capabilities: Principal["capabilities"]): Principal {
  return {
    kind: "api_key",
    userId,
    organizationId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
    teamId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
    role: "owner",
    subjectId: "api_key:e2e",
    credentialId: "e2e",
    authorizationEpoch: 1,
    capabilities,
  };
}

async function call(authenticated: Principal | undefined, body: unknown) {
  const request = new Request("https://nanocodex.example/v1/data", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  const response = await routeUserDataRequest(
    request,
    env as unknown as UserDataRouteEnv,
    new URL(request.url),
    async () => authenticated,
  );
  expect(response).toBeDefined();
  return { status: response!.status, body: await response!.json() };
}
