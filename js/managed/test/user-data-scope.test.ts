import { env } from "cloudflare:test";
import { describe, expect, it } from "vitest";

const USER_HEADER = "x-nanocodex-user-id";

describe("UserDataScope", () => {
  it("isolates accounts and provides optimistic JSON documents", async () => {
    const userA = crypto.randomUUID();
    const userB = crypto.randomUUID();
    const a = dataScope(userA);
    const b = dataScope(userB);
    await initialize(a, userA);
    await initialize(b, userB);

    const first = await operation(a, userA, {
      operation: "document_put",
      key: "whoop/profile",
      value: { strap: "fixture-a", settings: { max_hr: 190 } },
    });
    expect(first).toMatchObject({
      operation: "document_put",
      document: { key: "whoop/profile", version: 1, unchanged: false },
    });
    await expect(operation(a, userA, {
      operation: "document_put",
      key: "whoop/profile",
      value: { settings: { max_hr: 190 }, strap: "fixture-a" },
    })).resolves.toMatchObject({ document: { version: 1, unchanged: true } });
    await expect(operation(a, userA, {
      operation: "document_put",
      key: "whoop/profile",
      value: { strap: "fixture-a", settings: { max_hr: 191 } },
      if_version: 8,
    }, 409)).resolves.toMatchObject({ error: "conflict" });

    await expect(operation(a, userB, { operation: "document_get", key: "whoop/profile" }, 404))
      .resolves.toMatchObject({ error: "not_found" });
    await expect(operation(b, userB, { operation: "document_get", key: "whoop/profile" }, 404))
      .resolves.toMatchObject({ error: "not_found" });
    await expect(operation(a, userA, { operation: "document_list", prefix: "whoop/" }))
      .resolves.toMatchObject({ documents: [{ key: "whoop/profile", version: 1 }] });
  });

  it("writes idempotent time series, pages raw points, and aggregates buckets", async () => {
    const user = crypto.randomUUID();
    const scope = dataScope(user);
    await initialize(scope, user);
    const write = {
      operation: "timeseries_write",
      series: "whoop.heart_rate_bpm",
      points: [
        { timestamp_ms: 1_000, value: 60, fields: { source: "history" } },
        { timestamp_ms: 2_000, value: 70 },
        { timestamp_ms: 3_000, value: 80 },
      ],
    };
    await expect(operation(scope, user, write)).resolves.toMatchObject({ inserted: 3, replayed: 0 });
    await expect(operation(scope, user, write)).resolves.toMatchObject({ inserted: 0, replayed: 3 });
    await expect(operation(scope, user, { operation: "timeseries_list", prefix: "whoop." }))
      .resolves.toMatchObject({
        series: [{
          series: "whoop.heart_rate_bpm",
          points: 3,
          first_timestamp_ms: 1_000,
          last_timestamp_ms: 3_000,
        }],
      });
    await expect(operation(scope, user, {
      operation: "timeseries_write",
      series: "whoop.heart_rate_bpm",
      points: [{ timestamp_ms: 2_000, value: 71 }],
    }, 409)).resolves.toMatchObject({ error: "conflict" });

    const first = await operation(scope, user, {
      operation: "timeseries_query",
      series: "whoop.heart_rate_bpm",
      order: "asc",
      limit: 2,
    }) as { points: unknown[]; next_cursor: string };
    expect(first.points).toHaveLength(2);
    expect(first.next_cursor).toBe("2000");
    await expect(operation(scope, user, {
      operation: "timeseries_query",
      series: "whoop.heart_rate_bpm",
      order: "asc",
      cursor: first.next_cursor,
    })).resolves.toMatchObject({ points: [{ timestamp_ms: 3_000, value: 80 }] });
    await expect(operation(scope, user, {
      operation: "timeseries_aggregate",
      series: "whoop.heart_rate_bpm",
      start_ms: 1_000,
      end_ms: 3_000,
      bucket_ms: 2_000,
      aggregation: "avg",
    })).resolves.toMatchObject({
      buckets: [
        { start_ms: 1_000, value: 65, count: 2 },
        { start_ms: 3_000, value: 80, count: 1 },
      ],
    });
  });

  it("stores versioned opaque objects in R2 and verifies their digest", async () => {
    const user = crypto.randomUUID();
    const scope = dataScope(user);
    await initialize(scope, user);
    const put = await operation(scope, user, {
      operation: "object_put",
      key: "whoop/raw/batch-1.json",
      content: "{\"captures\":[]}",
      encoding: "utf8",
      content_type: "application/json",
      metadata: { integration: "whoop" },
    });
    expect(put).toMatchObject({ object: { version: 1, size_bytes: 15, unchanged: false } });
    await expect(operation(scope, user, {
      operation: "object_get",
      key: "whoop/raw/batch-1.json",
    })).resolves.toMatchObject({
      object: { content: "{\"captures\":[]}", encoding: "utf8", metadata: { integration: "whoop" } },
    });
    await expect(operation(scope, user, {
      operation: "object_put",
      key: "whoop/raw/batch-1.json",
      content: "{\"captures\":[]}",
      encoding: "utf8",
      content_type: "application/json",
      metadata: { integration: "whoop" },
    })).resolves.toMatchObject({ object: { version: 1, unchanged: true } });
    await expect(operation(scope, user, { operation: "object_list", prefix: "whoop/raw/" }))
      .resolves.toMatchObject({ objects: [{ key: "whoop/raw/batch-1.json", version: 1 }] });
    await expect(operation(scope, user, {
      operation: "object_delete", key: "whoop/raw/batch-1.json", if_version: 1,
    })).resolves.toMatchObject({ object: { version: 1 } });
    await expect(operation(scope, user, {
      operation: "object_get", key: "whoop/raw/batch-1.json",
    }, 404)).resolves.toMatchObject({ error: "not_found" });
  });
});

function dataScope(name: string): DurableObjectStub {
  return (env as unknown as { NANOCODEX_USER_DATA: DurableObjectNamespace })
    .NANOCODEX_USER_DATA.getByName(name);
}

async function initialize(scope: DurableObjectStub, user: string): Promise<void> {
  const response = await scope.fetch("https://user-data.internal/initialize", {
    method: "PUT",
    headers: { [USER_HEADER]: user },
  });
  expect(response.status).toBe(204);
}

async function operation(
  scope: DurableObjectStub,
  user: string,
  body: unknown,
  expectedStatus = 200,
): Promise<unknown> {
  const response = await scope.fetch("https://user-data.internal/operations", {
    method: "POST",
    headers: { "content-type": "application/json", [USER_HEADER]: user },
    body: JSON.stringify(body),
  });
  expect(response.status).toBe(expectedStatus);
  return response.json();
}
