import { env, runInDurableObject } from "cloudflare:test";
import { expect, it, vi } from "vitest";
import { DEFAULT_AGENT_SETTINGS } from "../src/agent-settings";
import { forwardPrincipalAssertions, type Principal } from "../src/account-auth";
import type { DurableAgentSession } from "../src/index";

const sdk = vi.hoisted(() => ({ getSandbox: vi.fn() }));
vi.mock("@cloudflare/sandbox", async (original) => ({ ...await original<typeof import("@cloudflare/sandbox")>(), getSandbox: sdk.getSandbox }));

// Exercise production admission, WASM tool dispatch, idle retirement and fresh
// Agent/tool assembly. Only inference and the external container RPC are faked.
it("resumes a CF process after managed runtime retirement without replay or retargeting", async () => {
  const grantId = `0x${"a".repeat(64)}`;
  const own: Principal = {
    kind: "connect_grant", userId: "11111111-1111-4111-8111-111111111111",
    organizationId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", teamId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
    role: "owner", subjectId: "user:process-fixture", credentialId: grantId, authorizationEpoch: 1,
    capabilities: ["agents:read", "agents:write", "tools:use"],
    connectGrant: { grantId, connectors: ["chatgpt"], mcpIds: [], sandboxExecution: true },
  };
  let principal = own;
  const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
  await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (session, state) => {
    let step = 0, turn = 0, publicId: number | undefined;
    let stdout = "FIRST_CHUNK::", terminal = false, processId = "";
    const transcript: any[] = [];
    const process = {
      get id() { return processId; }, get exitCode() { return terminal ? 0 : undefined; },
      getStatus: async () => terminal ? "completed" : "running",
      getLogs: async () => ({ stdout, stderr: "" }),
      kill: vi.fn(async () => {}),
    };
    const sandbox = {
      exec: vi.fn(async (cmd: string) => ({ success: true, exitCode: 0,
        stdout: cmd.includes("readlink") ? "linked" : cmd.includes("mountpoint") ? "mounted" : "", stderr: "" })),
      startProcess: vi.fn(async (_cmd: string, options: { processId: string }) => { processId = options.processId; return process; }),
      getProcess: vi.fn(async (id: string) => id === processId ? process : null),
      bindAccountEgress: vi.fn(async () => {}),
    };
    sdk.getSandbox.mockImplementation((_namespace, resource) => {
      expect(resource).toBe("nanocodex-retained-original");
      return sandbox;
    });
    class ModelSocket extends EventTarget {
      readyState = 1; bufferedAmount = 0;
      accept() {} close() { this.readyState = 3; }
      send(value: string) {
        const request = JSON.parse(value); transcript.push(request);
        const code = turn === 0
          ? `text(await tools.exec_command({cmd:"fixture-process",workdir:"/cloudflare-cad",yield_time_ms:0}));`
          : `text(await tools.write_stdin({session_id:${publicId},yield_time_ms:0}));`;
        const output = step++ === 0
          ? [{ type: "custom_tool_call", name: "exec", call_id: `process-${turn}`, input: code }]
          : [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "DONE" }] }];
        queueMicrotask(() => this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({
          type: "response.completed", response: { id: `response-${turn}-${step}`, status: "completed", output,
            usage: { input_tokens: 100, output_tokens: 10, total_tokens: 110 } },
        }) })));
      }
    }
    const original = (session as unknown as { env: Record<string, unknown> }).env;
    Object.defineProperty(session, "env", { configurable: true, value: { ...original,
      NANOCODEX: { fetch: async (input: RequestInfo | URL) => {
        const url = new URL(input instanceof Request ? input.url : String(input));
        if (url.pathname.includes("/responses")) return { status: 101, headers: new Headers(), webSocket: new ModelSocket() };
        if (url.pathname.startsWith("/subjects/")) return new Response(null, { status: 204 });
        return Response.json({ connectors: {}, mcp_connections: [], vault: [] });
      } },
      NANOCODEX_USERS: { getByName: () => ({ fetch: async () => new Response(null, { status: 204 }) }) },
      NANOCODEX_ACCOUNT_TOOLS: { getByName: () => ({ fetch: async () => Response.json({ tools: [], machines: [] }) }) },
      NANOCODEX_SANDBOXES: {},
    } });
    const request = (path: string, body?: unknown) => {
      const headers = new Headers();
      if (path !== "/create") forwardPrincipalAssertions(headers, principal);
      return session.fetch(new Request(`https://session.internal${path}`, {
        method: body === undefined ? "GET" : "POST", headers, ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      }));
    };
    const retire = async () => {
      state.storage.sql.exec("UPDATE session_state SET last_active=0");
      await session.alarm();
      expect(await (await request("/state")).json()).toMatchObject({ agent_loaded: false });
      await state.storage.deleteAlarm();
    };
    expect((await request("/create", {
      session_id: crypto.randomUUID(), owner_id: own.userId, organization_id: own.organizationId,
      team_id: own.teamId, authorization_epoch: 1, public_origin: "https://nanocodex.example",
      settings: DEFAULT_AGENT_SETTINGS, configuration: {},
    })).status).toBe(200);
    state.storage.sql.exec(`INSERT INTO managed_mounts VALUES ('mount-cad', 'cloudflare', 'cad', '/cloudflare-cad', 'retained-original', ?, 'mounted', 1, 1)`,
      JSON.stringify({ namespace_slot: 0, connect_grant_id: grantId }));
    try {
      for (turn = 0; turn < 7; turn++) {
        step = 0; transcript.length = 0;
        if (turn === 1) principal = { ...own, credentialId: `0x${"b".repeat(64)}`, connectGrant: { ...own.connectGrant!, grantId: `0x${"b".repeat(64)}` } };
        else if (turn === 3) state.storage.sql.exec("UPDATE managed_mounts SET provider_resource_id='replacement'");
        else if (turn === 4) { state.storage.sql.exec("UPDATE managed_mounts SET provider_resource_id='retained-original'"); principal = { ...own, connectGrant: { grantId, connectors: ["chatgpt"], mcpIds: [] } }; }
        else { principal = own; }
        if (turn === 2) stdout += "SECOND_CHUNK::";
        if (turn === 5) { stdout += "TAIL_CHUNK::"; terminal = true; }
        const reads = sandbox.getProcess.mock.calls.length;
        expect((await request("/turns", { id: `process-turn-${turn}`, input: "Exercise process fixture." })).status).toBe(202);
        await expect.poll(() => state.storage.sql.exec("SELECT state,error FROM managed_turns WHERE id=?", `process-turn-${turn}`).one(), { timeout: 20_000 })
          .toEqual({ state: "completed", error: null });
        const input = (transcript.at(-1)?.input ?? []).filter((item: any) => item.type === "custom_tool_call_output").map((item: any) => typeof item.output === "string" ? item.output : item.output.map((part: any) => part.text ?? "").join("\n")).join("\n");
        console.info({ type: "namespace.process.recovery", turn, input });
        if (turn === 0) {
          const matches = input.match(/session_id\\?":\s*(\d+)/);
          expect(matches).not.toBeNull(); publicId = Number(matches![1]);
          expect(input).toContain("FIRST_CHUNK::");
        } else if (turn === 2) {
          expect(input).toContain("SECOND_CHUNK::"); expect(input).not.toContain("FIRST_CHUNK::");
          expect(input).toContain(String(publicId));
        } else if (turn === 5) {
          expect(input).toContain("TAIL_CHUNK::"); expect(input).not.toContain("SECOND_CHUNK::");
          expect(input).toContain("exit_code");
        } else {
          expect(input).toMatch(/unknown or stale namespace process session|current authorization cannot use execution hands/);
          expect(sandbox.getProcess.mock.calls.length).toBe(reads);
        }
        await retire();
      }
      expect(sandbox.startProcess).toHaveBeenCalledTimes(1);
      expect(process.kill).not.toHaveBeenCalled();
    } finally { await retire(); sdk.getSandbox.mockReset(); }
  });
}, 120_000);
