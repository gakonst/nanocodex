import { env } from "cloudflare:test";
import { expect, it } from "vitest";
import { EXEC_COMMAND_PARAMETERS, EXECUTION_OUTPUT_SCHEMA } from "nanocodex-tools/execution-contract";
import { AccountHostedTools, AccountHostedToolsProvider } from "../src/account-hosted-tools";

// Regression from investigation session 01a0b165-c46a-7cde-bf09-f8f606143cf0.
it("thread repro: a hand replacement refreshes locally and replays the same effect receipt", async () => {
  const namespace = (env as unknown as { NANOCODEX_ACCOUNT_TOOLS: DurableObjectNamespace<AccountHostedTools> }).NANOCODEX_ACCOUNT_TOOLS;
  const owner = crypto.randomUUID();
  const stub = namespace.getByName(owner);
  const attach = async () => {
    const response = await stub.fetch("https://account-tools.internal/tool-host", {
      headers: { upgrade: "websocket", "x-nanocodex-owner-id": owner },
    });
    const socket = response.webSocket!;
    socket.accept();
    const ready = nextFrame(socket);
    socket.send(JSON.stringify({ type: "catalog", attachment_id: "repro-machine", tools: [machineEntry()],
      machines: [{ id: "repro-machine", name: "Repro machine", workspace: "/app", capabilities: ["shell"] }] }));
    await expect(ready).resolves.toEqual({ type: "ready" });
    return socket;
  };
  const first = await attach();
  const provider = new AccountHostedToolsProvider(namespace, owner, () => true);
  await provider.refresh();
  const staleTool = provider.machineTool("repro-machine", "exec_command")!;
  const successor = await attach();
  let dispatched = 0;
  successor.addEventListener("message", event => {
    const frame = JSON.parse(String(event.data));
    if (frame.type !== "call") return;
    dispatched++;
    successor.send(JSON.stringify({ type: "result", call_id: frame.call_id,
      outcome: { status: "completed", output: { output: "single effect receipt", success: true,
        structured_result: { output: "single effect receipt", exit_code: 0, wall_time_seconds: 0 },
        metadata: null, process_trace: null } } }));
  });
  const input = { cmd: "fixture-effect", workdir: "/app" };
  const context = { sessionId: "thread-repro", callId: "stable-effect-id" };
  try {
    await expect(staleTool.handler(input, context)).resolves.toMatchObject({ output: "single effect receipt" });
    // Reusing even the captured stale handler must reconcile to the same receipt.
    await expect(staleTool.handler(input, context)).resolves.toMatchObject({ output: "single effect receipt" });
    expect(dispatched).toBe(1);
  } finally { first.close(); successor.close(); }
}, 15_000);

function nextFrame(socket: WebSocket): Promise<Record<string, unknown>> {
  return new Promise((resolve, reject) => {
    const onMessage = (event: MessageEvent) => {
      cleanup();
      try { resolve(JSON.parse(String(event.data)) as Record<string, unknown>); }
      catch (error) { reject(error); }
    };
    const onError = () => {
      cleanup();
      reject(new Error("account Hosted Tools socket failed"));
    };
    const cleanup = () => {
      socket.removeEventListener("message", onMessage);
      socket.removeEventListener("error", onError);
    };
    socket.addEventListener("message", onMessage);
    socket.addEventListener("error", onError);
  });
}

function machineEntry() {
  return {
    provider: "machine",
    remote_name: "exec_command",
    definition: {
      type: "function" as const,
      name: "exec_command",
      description: "Canonical machine exec_command",
      strict: false,
      parameters: EXEC_COMMAND_PARAMETERS,
      output_schema: EXECUTION_OUTPUT_SCHEMA,
    },
    parallel_safe: true,
    summary: "Machine exec_command",
    timeout_ms: 30_000,
  };
}

