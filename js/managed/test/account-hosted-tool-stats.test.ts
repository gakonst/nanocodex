import { createExecutionContext, env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import worker, { type AccountHostedTools } from "../src/index";
import type { Principal } from "../src/account-auth";
import { SqlHostedToolsPersistence } from "../src/hosted-tools-broker";

function fixture() {
  const owner = crypto.randomUUID();
  const principal: Principal = {
    kind: "api_key", userId: owner, organizationId: crypto.randomUUID(), teamId: crypto.randomUUID(),
    role: "owner", subjectId: `user:${owner}`, credentialId: "test", authorizationEpoch: 1,
    capabilities: ["agents:read", "tools:use"],
  };
  const namespace = (env as unknown as {
    NANOCODEX_ACCOUNT_TOOLS: DurableObjectNamespace<AccountHostedTools>;
  }).NANOCODEX_ACCOUNT_TOOLS;
  const stub = namespace.getByName(owner);
  const call = (actor: Principal | undefined = principal, method = "GET", suffix = "") => worker.fetch(
    new Request(`https://nanocodex.example/v1/account/hosted-tool-stats${suffix}`, { method }),
    env as Parameters<typeof worker.fetch>[1], createExecutionContext(), actor,
  );
  return { owner, principal, stub, namespace, call };
}

describe("owner hosted tool statistics", () => {
  it("groups unique persisted calls from the last 24 hours by tool/state and terminal duration without private fields", async () => {
    const { owner, principal, stub, namespace, call } = fixture();
    const empty = await call(); // Claims a fresh account and initializes the broker table.
    expect(empty.status).toBe(200);
    expect(await empty.json()).toMatchObject({ total_calls: 0, data: [] });
    const now = Date.now();
    await runInDurableObject(stub, async (_, state) => {
      expect(state.storage.sql.exec<{ name: string }>("PRAGMA index_list(hosted_tool_calls)")
        .toArray().map(index => index.name)).toContain("hosted_tool_calls_created_at");
      const insert = (source: string, name: string, callState: string, created: number, updated: number) => {
        state.storage.sql.exec(`INSERT INTO hosted_tool_calls
          (call_id, session_id, source_call_id, host_id, lease_id, generation, model, name,
           input_json, output_token_budget, output_byte_budget, deadline_at, cancel_requested,
           state, result_json, receipt_json, created_at, updated_at)
          VALUES (?, ?, ?, ?, ?, 1, ?, ?, ?, 1, 1, ?, 0, ?, ?, ?, ?, ?)`,
          source, "private-session", source, "private-host", "private-lease", "fixture", name,
          'PRIVATE_INPUT', created + 10_000, callState, JSON.stringify({ status: 'completed', output: { success: true, output: 'PRIVATE_RESULT' } }), 'PRIVATE_RECEIPT', created, updated);
      };
      insert("a", "exec_command", "completed", now - 1000, now - 900);
      insert("b", "exec_command", "completed", now - 900, now - 700);
      state.storage.sql.exec("UPDATE hosted_tool_calls SET result_json = ? WHERE call_id = ?",
        JSON.stringify({ status: "completed", output: { success: false, output: "PRIVATE_RESULT",
          structured_result: { status: "ambiguous" } } }), "b");
      insert("c", "exec_command", "ambiguous", now - 500, now - 460);
      insert("d", "exec_command", "dispatched", now - 400, now - 300);
      insert("e", "mcp__cua_repl__js", "unavailable", now - 200, now - 190);
      insert("f", "mcp__cua_repl__js", "unavailable", now - 160, now - 140);
      state.storage.sql.exec("UPDATE hosted_tool_calls SET dispatched_at = ? WHERE call_id = ?", now - 155, "f");
      insert("g", "mcp__cua_repl__js", "completed", now - 120, now - 100);
      state.storage.sql.exec("UPDATE hosted_tool_calls SET result_json = ? WHERE call_id = ?",
        JSON.stringify({ status: "completed", output: { success: false, output: "PRIVATE_RESULT",
          structured_result: { status: "unavailable" } } }), "g");
      insert("h", "mcp__cua_repl__js", "completed", now - 80, now - 70);
      state.storage.sql.exec("UPDATE hosted_tool_calls SET result_json = ? WHERE call_id = ?",
        JSON.stringify({ status: "completed", output: { success: false, output: "PRIVATE_RESULT",
          structured_result: { status: "provider_error" } } }), "h");
      insert("old", "exec_command", "completed", now - 86_400_100, now - 86_400_000);
      insert("future", "exec_command", "completed", now + 60_000, now + 60_100);
      expect(() => insert("a", "exec_command", "completed", now - 1000, now - 900)).toThrow();
    });
    const response = await call();
    expect(response.status).toBe(200);
    expect(response.headers.get("cache-control")).toBe("no-store");
    const payload = await response.json<{
      window: { from: number; to: number }; total_calls: number; data: Record<string, unknown>[];
    }>();
    expect(payload.window.to - payload.window.from).toBe(86_400_000);
    expect(payload.total_calls).toBe(8);
    expect(payload.data).toEqual([
      { name: "exec_command", state: "ambiguous", calls: 1, tool_failed: 0,
        tool_ambiguous: 0, tool_unavailable: 0, tool_failed_other: 0, late_receipts: 1,
        pre_dispatch_unavailable: 0, post_dispatch_unavailable: 0, unknown_dispatch_unavailable: 0, duration_count: 1,
        total_duration_ms: 40, avg_duration_ms: 40, min_duration_ms: 40, max_duration_ms: 40 },
      { name: "exec_command", state: "completed", calls: 2, tool_failed: 1,
        tool_ambiguous: 1, tool_unavailable: 0, tool_failed_other: 0, late_receipts: 0,
        pre_dispatch_unavailable: 0, post_dispatch_unavailable: 0, unknown_dispatch_unavailable: 0, duration_count: 2,
        total_duration_ms: 300, avg_duration_ms: 150, min_duration_ms: 100, max_duration_ms: 200 },
      { name: "exec_command", state: "dispatched", calls: 1, tool_failed: 0,
        tool_ambiguous: 0, tool_unavailable: 0, tool_failed_other: 0, late_receipts: 0,
        pre_dispatch_unavailable: 0, post_dispatch_unavailable: 0, unknown_dispatch_unavailable: 0, duration_count: 0,
        total_duration_ms: null, avg_duration_ms: null, min_duration_ms: null, max_duration_ms: null },
      { name: "mcp__cua_repl__js", state: "completed", calls: 2, tool_failed: 2,
        tool_ambiguous: 0, tool_unavailable: 1, tool_failed_other: 1, late_receipts: 0,
        pre_dispatch_unavailable: 0, post_dispatch_unavailable: 0, unknown_dispatch_unavailable: 0, duration_count: 2,
        total_duration_ms: 30, avg_duration_ms: 15, min_duration_ms: 10, max_duration_ms: 20 },
      { name: "mcp__cua_repl__js", state: "unavailable", calls: 2, tool_failed: 0,
        tool_ambiguous: 0, tool_unavailable: 0, tool_failed_other: 0, late_receipts: 0,
        pre_dispatch_unavailable: 1, post_dispatch_unavailable: 1, unknown_dispatch_unavailable: 0, duration_count: 2,
        total_duration_ms: 30, avg_duration_ms: 15, min_duration_ms: 10, max_duration_ms: 20 },
    ]);
    const serialized = JSON.stringify(payload);
    for (const privateValue of ["private-session", "private-host", "private-lease", "PRIVATE_INPUT",
      "PRIVATE_RESULT", "PRIVATE_RECEIPT", "source_call_id", "call_id", "future", "old"]) {
      expect(serialized).not.toContain(privateValue);
    }
    expect(await (await call({ ...principal, userId: crypto.randomUUID() })).json()).toMatchObject({ total_calls: 0 });
    const forged = await namespace.getByName(owner).fetch("https://account-tools.internal/hosted-tool-stats", {
      headers: { "x-nanocodex-owner-id": crypto.randomUUID() },
    });
    expect(forged.status).toBe(404);
  });

  it("keeps legacy dispatch status unknown rather than misclassifying pre-dispatch", async () => {
    const { stub, call } = fixture();
    expect((await call()).status).toBe(200);
    const now = Date.now();
    await runInDurableObject(stub, async (_, state) => {
      state.storage.sql.exec("ALTER TABLE hosted_tool_calls DROP COLUMN dispatched_at");
      state.storage.sql.exec(`INSERT INTO hosted_tool_calls
        (call_id, session_id, source_call_id, host_id, lease_id, generation, model, name,
         input_json, output_token_budget, output_byte_budget, deadline_at, cancel_requested,
         state, result_json, receipt_json, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, 1, ?, ?, ?, 1, 1, ?, 0, 'unavailable', ?, NULL, ?, ?)`,
        "legacy", "private-session", "legacy", "private-host", "private-lease", "fixture", "exec_command",
        "PRIVATE_INPUT", now + 1000, JSON.stringify({ status: "unavailable", message: "PRIVATE_RESULT" }), now - 100, now - 50);
      new SqlHostedToolsPersistence(state.storage).initialize(now);
      expect(state.storage.sql.exec<{ dispatched_at: number | null }>(
        "SELECT dispatched_at FROM hosted_tool_calls WHERE call_id = 'legacy'",
      ).toArray()[0]?.dispatched_at).toBeNull();
    });
    const payload = await (await call()).json<{ data: { name: string; state: string;
      pre_dispatch_unavailable: number; post_dispatch_unavailable: number; unknown_dispatch_unavailable: number }[] }>();
    expect(payload.data).toContainEqual(expect.objectContaining({ name: "exec_command", state: "unavailable",
      pre_dispatch_unavailable: 0, post_dispatch_unavailable: 0, unknown_dispatch_unavailable: 1 }));
  });

  it("rejects unauthenticated readers, Connect grants, missing permissions, writes and selectors", async () => {
    const { principal, call } = fixture();
    expect((await worker.fetch(new Request("https://nanocodex.example/v1/account/hosted-tool-stats"),
      env as Parameters<typeof worker.fetch>[1], createExecutionContext())).status).toBe(401);
    expect((await call({ ...principal, kind: "connect_grant" })).status).toBe(403);
    expect((await call({ ...principal, connectGrant: { grantId: "grant" } as NonNullable<Principal["connectGrant"]> })).status).toBe(403);
    for (const capabilities of [[], ["agents:read"], ["tools:use"]] as Principal["capabilities"][]) {
      expect((await call({ ...principal, capabilities })).status).toBe(403);
    }
    expect((await call(principal, "POST")).status).toBe(405);
    expect((await call(principal, "GET", "?owner=someone-else")).status).toBe(400);
  });
});
