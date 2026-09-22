import { createExecutionContext, env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import worker, { type DurableAgentSession } from "../src/index";
import { downloadPath, downloadHandFile, fileReadCommand } from "../src/file-download";
import type { Principal } from "../src/account-auth";

const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
const bucket = (env as unknown as { NANOCODEX_WORKSPACES: R2Bucket }).NANOCODEX_WORKSPACES;
const context = () => ({ sessionId: crypto.randomUUID(), callId: crypto.randomUUID(), parentCallId: "", model: "file-download", signal: new AbortController().signal });

it("authenticates file reads and streams the exact brain bytes from the owning conversation", async () => {
  const id = crypto.randomUUID();
  const principal: Principal = { kind: "api_key", userId: crypto.randomUUID(), organizationId: crypto.randomUUID(), teamId: crypto.randomUUID(),
    role: "owner", subjectId: "api_key:file-test", credentialId: "file-test", authorizationEpoch: 1, capabilities: ["agents:read", "tools:use"] };
  await runInDurableObject(sessions.getByName(id), async (session, ctx) => {
    ctx.storage.sql.exec(`INSERT INTO session_state (singleton, session_id, owner_id, organization_id, team_id,
      authorization_epoch, public_origin, runtime_profile, last_active) VALUES (1, ?, ?, ?, ?, 1, 'https://nanocodex.example', 'managed', ?)`,
    id, principal.userId, principal.organizationId, principal.teamId, Date.now());
    expect((await session.fetch(new Request("https://session.internal/files?path=/brain/secret"))).status).toBe(404);
  });
  const path = "/brain/outputs/Some file #?ü.zip";
  const bytes = new Uint8Array([0, 255, 80, 75, 128, 10]);
  await bucket.put(`brains/${id}/${path.slice(7)}`, bytes);
  const call = (actor = principal, target = path, method = "GET") => worker.fetch(
    new Request(`https://nanocodex.example/v1/agents/${id}/files?${new URLSearchParams({ path: target })}`, { method }),
    env as Parameters<typeof worker.fetch>[1], createExecutionContext(), actor,
  );
  const response = await call();
  expect(response.status).toBe(200);
  expect(response.headers.get("cache-control")).toBe("private, no-store");
  expect(response.headers.get("content-disposition")).toContain("Some%20file%20%23%3F%C3%BC.zip");
  expect(new Uint8Array(await response.arrayBuffer())).toEqual(bytes);
  for (const actor of [{ ...principal, userId: crypto.randomUUID() }, { ...principal, teamId: crypto.randomUUID() },
    { ...principal, authorizationEpoch: 2 }, { ...principal, capabilities: ["agents:read"] as const }, { ...principal, capabilities: ["tools:use"] as const }]) {
    expect((await call(actor)).status).not.toBe(200);
  }
  expect((await call({ ...principal, connectGrant: { grantId: `0x${"a".repeat(64)}`, connectors: [], mcpIds: [] } })).status).toBe(403);
  expect((await call(principal, path, "POST")).status).toBe(405);
  expect((await call(principal, "/brain/missing")).status).toBe(404);
  expect((await call(principal, "/brain/../some-hand/private")).status).toBe(400);
  const local = await call(principal, "/Users/me/source.rs");
  expect(local.status).toBe(404);
  expect(await local.json()).toMatchObject({ error: "file_path_unmapped" });
  await runInDurableObject(sessions.getByName(id), async (_session, ctx) => {
    ctx.storage.sql.exec("INSERT INTO managed_hand_paths(machine_id, root) VALUES (?, ?)", "offline-box", "/offline-box");
  });
  const offline = await call(principal, "/offline-box/output.zip");
  expect(offline.status).toBe(503);
  expect(await offline.json()).toMatchObject({ error: "hand_unavailable" });
});

it("does not permit ambiguous or traversing download paths", () => {
  for (const path of ["relative", "/brain/../other/file", "/brain/./file", "/brain//file", "/brain/a\0b", "/brain/", "/brain\\file"]) {
    expect(() => downloadPath(new URL(`https://example.com/files?${new URLSearchParams({ path })}`))).toThrow();
  }
  expect(() => downloadPath(new URL("https://example.com/files?path=/brain/a&path=/brain/b"))).toThrow();
  expect(downloadPath(new URL("https://example.com/files?path=%2Fbrain%2Fa%2520b"))).toBe("/brain/a%20b");
});

it("streams bounded Hand chunks from a captured route and fails on truncated results", async () => {
  const size = 192 * 1024 + 2;
  const calls: unknown[] = [];
  let chunk = 0;
  const exec = { handler: async (input: unknown) => {
    calls.push(input);
    return { exit_code: 0, output: `${size}\n${chunk++ === 0 ? btoa("a".repeat(192 * 1024)) : btoa("\0\xff")}` };
  } };
  const response = await downloadHandFile("/remote/delivery/a.zip", "/Users/me/work", "/remote", exec, context(), () => true);
  const bytes = new Uint8Array(await response.arrayBuffer());
  expect(bytes.length).toBe(size);
  expect([...bytes.slice(-2)]).toEqual([0, 255]);
  expect(calls).toHaveLength(2);
  expect(calls[0]).toMatchObject({ workdir: "/Users/me/work", shell: "/bin/bash", login: false });
  expect((calls[0] as { cmd: string }).cmd).toContain("'/Users/me/work/delivery/a.zip'");
  await expect(downloadHandFile("/remote/a", "/work", "/remote", { handler: async () => ({ exit_code: 0, output: "5\nYQ==" }) }, context(), () => true))
    .rejects.toMatchObject({ code: "file_read_failed" });
  await expect(downloadHandFile("/remote/a", "/work", "/remote", { handler: async () => ({ exit_code: 44, output: "" }) }, context(), () => true))
    .rejects.toMatchObject({ code: "file_not_found" });
});

it("quotes filenames as data on POSIX and Windows and stops after deletion", async () => {
  expect(fileReadCommand("/work/a'$(touch /tmp/oops).zip", 0, false)).toContain("'/work/a'\"'\"'$(touch /tmp/oops).zip'");
  expect(fileReadCommand("C:/work/a'$(boom).zip", 0, true)).toContain("'C:/work/a''$(boom).zip'");
  let called = false;
  await expect(downloadHandFile("/remote/a", "/work", "/remote", { handler: async () => { called = true; } }, context(), () => false))
    .rejects.toMatchObject({ code: "agent_unavailable" });
  expect(called).toBe(false);
});

it("downloads from the account Hand while the conversation has no active model turn", async () => {
  const { EXEC_COMMAND_PARAMETERS, EXECUTION_OUTPUT_SCHEMA } = await import("nanocodex-tools/execution-contract");
  const id = crypto.randomUUID(), owner = crypto.randomUUID();
  const principal: Principal = { kind: "api_key", userId: owner, organizationId: crypto.randomUUID(), teamId: crypto.randomUUID(),
    role: "owner", subjectId: "api_key:file-hand-test", credentialId: "file-hand-test", authorizationEpoch: 1, capabilities: ["agents:read", "tools:use"] };
  await runInDurableObject(sessions.getByName(id), async (_session, ctx) => {
    ctx.storage.sql.exec(`INSERT INTO session_state (singleton, session_id, owner_id, organization_id, team_id,
      authorization_epoch, public_origin, runtime_profile, last_active) VALUES (1, ?, ?, ?, ?, 1, 'https://nanocodex.example', 'managed', ?)`,
    id, owner, principal.organizationId, principal.teamId, Date.now());
  });
  const namespace = (env as unknown as { NANOCODEX_ACCOUNT_TOOLS: DurableObjectNamespace }).NANOCODEX_ACCOUNT_TOOLS;
  const upgrade = await namespace.getByName(owner).fetch("https://account-tools.internal/tool-host", {
    headers: { upgrade: "websocket", "x-nanocodex-owner-id": owner },
  });
  const socket = upgrade.webSocket!;
  socket.accept();
  try {
    const ready = new Promise(resolve => socket.addEventListener("message", event => resolve(JSON.parse(String(event.data))), { once: true }));
    socket.send(JSON.stringify({ type: "catalog", capabilities: ["turn_metadata"], attachment_id: "remote-machine", machines: [{
      id: "remote-machine", name: "Omarchy Desktop", workspace: "/srv/remote", capabilities: ["native", "filesystem", "shell"],
    }], tools: [{ provider: "machine", remote_name: "exec_command", parallel_safe: true, timeout_ms: 30_000,
      definition: { type: "function", name: "exec_command", description: "Read fixture", strict: false,
        parameters: EXEC_COMMAND_PARAMETERS, output_schema: EXECUTION_OUTPUT_SCHEMA } }] }));
    expect(await ready).toEqual({ type: "ready" });
    const calls: Record<string, unknown>[] = [];
    socket.addEventListener("message", event => {
      const frame = JSON.parse(String(event.data));
      if (frame.type !== "call") return;
      calls.push(frame);
      const output = "4\nAP9QSw==";
      socket.send(JSON.stringify({ type: "result", call_id: frame.call_id, outcome: { status: "completed", output: {
        output, success: true, structured_result: { output, exit_code: 0, wall_time_seconds: 0 }, metadata: null, process_trace: null,
      } } }));
    });
    const response = await worker.fetch(new Request(`https://nanocodex.example/v1/agents/${id}/files?path=/omarchy-desktop/gpu-stack/a.zip`),
      env as Parameters<typeof worker.fetch>[1], createExecutionContext(), principal);
    expect(response.status).toBe(200);
    expect(new Uint8Array(await response.arrayBuffer())).toEqual(new Uint8Array([0, 255, 80, 75]));
    expect(calls).toHaveLength(1);
    expect(calls[0]).toMatchObject({ name: "exec_command", input: { workdir: "/srv/remote" } });
    expect((calls[0]!.input as { cmd: string }).cmd).toContain("'/srv/remote/gpu-stack/a.zip'");
  } finally { socket.close(1000, "done"); }
});
