import { describe, expect, it } from "vitest";
import { hostedToolCatalogDigest } from "nanocodex/tools/hosted-catalog";

import {
  HostedToolsBroker,
  HOSTED_TOOLS_PRE_ADMISSION_UNAVAILABLE,
  type HostedToolsBrokerContext,
  type HostedToolsBrokerPersistence,
} from "../src/hosted-tools-broker";
import {
  hostedToolCatalogEntryAllowed,
} from "../src/app-tool-catalog";
import type { HostedToolCatalogEntry } from "../src/hosted-tools-protocol";

const NOW = 1_000_000;
const GRANT_A = `0x${"a".repeat(64)}`;
const GRANT_B = `0x${"b".repeat(64)}`;
const IDS = [
  "11111111-1111-4111-8111-111111111111",
  "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
  "22222222-2222-4222-8222-222222222222",
  "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
];
const CLEANUP_DIGEST = await hostedToolCatalogDigest([cleanupEntry()]);

type State = ReturnType<HostedToolsBrokerPersistence["state"]>;
type CallRow = NonNullable<ReturnType<HostedToolsBrokerPersistence["call"]>>;
type CallState = CallRow["state"];

describe("HostedToolsBroker socket-owned protocol", () => {
  it("accepts one immutable catalog and exposes provider metadata without public identity pins", async () => {
    const fixture = createFixture();
    const host = fixture.socket();
    await catalog(fixture.broker, host);

    expect(host.sent).toEqual([{ type: "ready" }]);
    expect(fixture.broker.provider().definitions()).toEqual([
      expect.objectContaining({ name: "fixture__lookup", defer_loading: true }),
    ]);
    expect(fixture.broker.provider().resolve("fixture__lookup")).toMatchObject({
      name: "fixture__lookup",
      provider: "fixture",
      remoteName: "lookup",
      summary: "Fixture lookup",
    });
    expect(Object.keys(host.sent[0]!)).toEqual(["type"]);

    await catalog(fixture.broker, host);
    expect(host.closed).toMatchObject({ code: 1008 });
    expect(host.sent).not.toContainEqual(expect.objectContaining({ type: "fenced" }));
  });

  it("projects account machines only while their host is routing-ready", async () => {
    const fixture = createFixture();
    const host = fixture.socket();
    const machines = [{
      id: "desktop",
      name: "Build desktop",
      workspace: "/home/george/repo",
      capabilities: ["filesystem", "native-shell"],
    }];
    await fixture.broker.message(host.webSocket, JSON.stringify({
      type: "catalog",
      tools: [entry()],
      machines,
    }));

    expect(fixture.broker.machines()).toEqual(machines);
    await fixture.broker.message(host.webSocket, JSON.stringify({ type: "drain" }));
    expect(fixture.broker.machines()).toEqual([]);
  });

  it("replaces the live machine snapshot without rebuilding its broker", async () => {
    const fixture = createFixture();
    const first = fixture.socket();
    await fixture.broker.message(first.webSocket, JSON.stringify({
      type: "catalog",
      tools: [entry()],
      machines: [{
        id: "laptop",
        name: "Laptop",
        workspace: "/Users/george/repo",
        capabilities: ["filesystem"],
      }],
    }));
    const replacement = fixture.socket();
    await fixture.broker.message(replacement.webSocket, JSON.stringify({
      type: "catalog",
      tools: [entry()],
      machines: [{
        id: "desktop",
        name: "Desktop",
        workspace: "/home/george/repo",
        capabilities: ["filesystem", "native-shell"],
      }],
    }));

    expect(first.closed).toMatchObject({ code: 1008 });
    expect(fixture.broker.machines()).toEqual([{
      id: "desktop",
      name: "Desktop",
      workspace: "/home/george/repo",
      capabilities: ["filesystem", "native-shell"],
    }]);
  });

  it("removes machines when an open host lease expires", async () => {
    const fixture = createFixture();
    const host = fixture.socket();
    await fixture.broker.message(host.webSocket, JSON.stringify({
      type: "catalog",
      tools: [entry()],
      machines: [{
        id: "laptop",
        name: "Laptop",
        workspace: "/workspace",
        capabilities: ["filesystem"],
      }],
    }));
    fixture.persistence.current.lease_expires_at = NOW;

    expect(fixture.broker.machines()).toEqual([]);
    expect(host.closed).toMatchObject({ code: 1008 });
  });

  it("rejects machine metadata from Connect-grant hosts", async () => {
    const fixture = createFixture();
    const host = fixture.socket([], CLEANUP_DIGEST, GRANT_A);
    await fixture.broker.message(host.webSocket, JSON.stringify({
      type: "catalog",
      tools: [cleanupEntry()],
      machines: [{
        id: "desktop",
        name: "Build desktop",
        workspace: "/home/george/repo",
        capabilities: ["filesystem"],
      }],
    }));

    expect(host.closed).toMatchObject({
      code: 1008,
      reason: expect.stringContaining("catalog_contract_mismatch"),
    });
    expect(fixture.broker.machines()).toEqual([]);
  });

  it("durably dispatches an exact call and ACKs both the result and duplicate receipt", async () => {
    const fixture = createFixture();
    const host = fixture.socket();
    await catalog(fixture.broker, host);
    const tool = fixture.broker.provider().resolve("fixture__lookup")!;
    const pending = tool.handler({ id: "42" }, {
      sessionId: "session:1",
      callId: "source:1",
      model: "gpt-5.2",
    });
    const call = host.sent.find((frame) => frame.type === "call")!;
    expect(call).toEqual({
      type: "call",
      session_id: "session:1",
      call_id: IDS[1],
      model: "gpt-5.2",
      name: "fixture__lookup",
      input: { id: "42" },
      output_token_budget: 10_000,
      output_byte_budget: 128 * 1024,
      deadline_at: NOW + 30_000,
    });
    await fixture.broker.message(host.webSocket, result(IDS[1]!, "done"));
    await expect(pending).resolves.toMatchObject({ success: true, output: "done" });
    expect(host.sent.at(-1)).toEqual({ type: "ack", call_id: IDS[1] });
    await fixture.broker.message(host.webSocket, result(IDS[1]!, "done"));
    expect(host.sent.filter((frame) => frame.type === "ack")).toHaveLength(2);
    expect(fixture.persistence.call(IDS[1]!)?.state).toBe("completed");
  });

  it("removes routing before acknowledging graceful drain while dispatched calls can finish", async () => {
    const fixture = createFixture();
    const host = fixture.socket();
    await catalog(fixture.broker, host);
    const tool = fixture.broker.provider().resolve("fixture__lookup")!;
    const pending = tool.handler({}, { sessionId: "session:1", callId: "source:1" });
    let definitionsAtAck = -1;
    host.onSend = (frame) => {
      if (frame.type === "draining") definitionsAtAck = fixture.broker.provider().definitions().length;
    };
    await fixture.broker.message(host.webSocket, JSON.stringify({ type: "drain" }));
    expect(definitionsAtAck).toBe(0);
    expect(host.sent.at(-1)).toEqual({ type: "draining" });
    expect(fixture.broker.provider().resolve("fixture__lookup")).toBeUndefined();

    await fixture.broker.message(host.webSocket, result(IDS[1]!, "after drain"));
    await expect(pending).resolves.toMatchObject({ success: true, output: "after drain" });
    expect(host.sent.at(-1)).toEqual({ type: "ack", call_id: IDS[1] });
  });

  it("does not admit a handler selected before the drain barrier", async () => {
    const fixture = createFixture();
    const host = fixture.socket();
    await catalog(fixture.broker, host);
    const selected = fixture.broker.provider().resolve("fixture__lookup")!;
    await fixture.broker.message(host.webSocket, JSON.stringify({ type: "drain" }));

    const outcome = await selected.handler({}, { sessionId: "session:1", callId: "source:1" });
    expect((outcome as Record<PropertyKey, unknown>)[HOSTED_TOOLS_PRE_ADMISSION_UNAVAILABLE]).toBe(true);
    expect(host.sent.some((frame) => frame.type === "call")).toBe(false);
    expect(fixture.persistence.callBySource("session:1", "source:1")).toBeUndefined();
  });

  it("keeps the active catalog when a replacement candidate fails parity validation", async () => {
    const fixture = createFixture();
    const first = fixture.socket();
    await catalog(fixture.broker, first);
    fixture.broker.provider().setCatalogValidator((definitions) => {
      if (definitions[0]?.provider === "rejected") throw new Error("same-name parity failed");
      return true;
    });
    const candidate = fixture.socket();
    await fixture.broker.message(candidate.webSocket, JSON.stringify({
      type: "catalog",
      tools: [{ ...entry(), provider: "rejected" }],
    }));
    expect(candidate.closed).toMatchObject({ code: 1008, reason: expect.stringContaining("parity failed") });
    expect(first.closed).toBeUndefined();
    expect(fixture.broker.provider().resolve("fixture__lookup")).toBeDefined();
  });

  it("accepts only the exact grant-bound app catalog and signed MCP providers", async () => {
    const mcpId = "m".repeat(43);
    const allowed = createFixture();
    const allowedHost = allowed.socket([mcpId], CLEANUP_DIGEST, GRANT_A);
    await allowed.broker.message(allowedHost.webSocket, JSON.stringify({
      type: "catalog",
      tools: [cleanupEntry(), { ...entry(), provider: `mcp:${mcpId}` }],
    }));
    expect(allowedHost.closed).toBeUndefined();
    expect(allowedHost.sent).toEqual([{ type: "ready" }]);

    for (const [candidate, digest] of [
      [cleanupEntry(), undefined],
      [cleanupEntry("other"), CLEANUP_DIGEST],
      [cleanupEntry("cleanup", true), CLEANUP_DIGEST],
      [{ ...entry(), provider: `mcp:${"x".repeat(43)}` }, CLEANUP_DIGEST],
    ] as const) {
      const denied = createFixture();
      const deniedHost = denied.socket([mcpId], digest, GRANT_A);
      await denied.broker.message(deniedHost.webSocket, JSON.stringify({
        type: "catalog",
        tools: [candidate],
      }));
      expect(deniedHost.closed).toMatchObject({
        code: 1008,
        reason: expect.stringContaining("catalog_contract_mismatch"),
      });
      expect(denied.broker.provider().definitions()).toEqual([]);
    }
  });

  it("rejects cross-grant replacement and hides the retained host from another grant's turn", async () => {
    const mcpId = "m".repeat(43);
    let activeGrantId = GRANT_A;
    const fixture = createFixture((candidate, hostGrantId, hostDigest) => hostedToolCatalogEntryAllowed({
      grantId: activeGrantId,
      mcpIds: [mcpId],
      appToolCatalogDigest: CLEANUP_DIGEST,
    }, hostGrantId, hostDigest, candidate));
    const first = fixture.socket([mcpId], CLEANUP_DIGEST, GRANT_A);
    await fixture.broker.message(first.webSocket, JSON.stringify({
      type: "catalog",
      tools: [cleanupEntry(), { ...entry(), provider: `mcp:${mcpId}` }],
    }));
    expect(first.sent).toEqual([{ type: "ready" }]);
    expect(fixture.broker.provider().definitions()).toHaveLength(2);
    const selected = fixture.broker.provider().resolve("cleanup")!;

    const competing = fixture.socket([mcpId], CLEANUP_DIGEST, GRANT_B);
    await fixture.broker.message(competing.webSocket, JSON.stringify({
      type: "catalog",
      tools: [cleanupEntry(), { ...entry(), provider: `mcp:${mcpId}` }],
    }));
    expect(competing.closed).toMatchObject({
      code: 1008,
      reason: expect.stringContaining("grant_conflict"),
    });
    expect(first.closed).toBeUndefined();

    activeGrantId = GRANT_B;
    expect(fixture.broker.provider().definitions()).toEqual([]);
    await expect(selected.handler({}, { sessionId: "session:1", callId: "source:1" }))
      .resolves.toMatchObject({
        success: false,
        structuredResult: { status: "unavailable" },
      });
    expect(first.sent.some((frame) => frame.type === "call")).toBe(false);
    expect(competing.sent.some((frame) => frame.type === "call")).toBe(false);
  });

  it("rejects replacement across Connect and account host boundaries in both directions", async () => {
    for (const connectFirst of [true, false]) {
      const fixture = createFixture();
      const first = connectFirst
        ? fixture.socket([], CLEANUP_DIGEST, GRANT_A)
        : fixture.socket();
      await fixture.broker.message(first.webSocket, JSON.stringify({
        type: "catalog",
        tools: [connectFirst ? cleanupEntry() : entry()],
      }));
      expect(first.sent).toEqual([{ type: "ready" }]);

      const competing = connectFirst
        ? fixture.socket()
        : fixture.socket([], CLEANUP_DIGEST, GRANT_A);
      await fixture.broker.message(competing.webSocket, JSON.stringify({
        type: "catalog",
        tools: [connectFirst ? entry() : cleanupEntry()],
      }));
      expect(competing.closed).toMatchObject({
        code: 1008,
        reason: expect.stringContaining("grant_conflict"),
      });
      expect(first.closed).toBeUndefined();
    }
  });

  it("does not project a Connect host to an account turn or an account host to a Connect turn", () => {
    const mcpId = "m".repeat(43);
    const connectGrant = {
      grantId: GRANT_A,
      mcpIds: [mcpId],
      appToolCatalogDigest: CLEANUP_DIGEST,
    } as const;
    expect(hostedToolCatalogEntryAllowed(undefined, GRANT_A, CLEANUP_DIGEST, cleanupEntry())).toBe(false);
    expect(hostedToolCatalogEntryAllowed(connectGrant, undefined, CLEANUP_DIGEST, cleanupEntry())).toBe(false);
    expect(hostedToolCatalogEntryAllowed(undefined, undefined, undefined, entry())).toBe(true);
    expect(hostedToolCatalogEntryAllowed(connectGrant, GRANT_A, CLEANUP_DIGEST, cleanupEntry())).toBe(true);
    expect(hostedToolCatalogEntryAllowed(
      connectGrant,
      GRANT_A,
      CLEANUP_DIGEST,
      { ...entry(), provider: `mcp:${mcpId}` },
    )).toBe(true);
  });

  it("blocks a previously selected cleanup tool when the active catalog grant changes", async () => {
    let digest: string | undefined = CLEANUP_DIGEST;
    const fixture = createFixture((candidate, hostGrantId, hostDigest) => hostedToolCatalogEntryAllowed(
      digest === undefined ? undefined : { grantId: GRANT_A, mcpIds: [], appToolCatalogDigest: digest as `0x${string}` },
      hostGrantId,
      hostDigest,
      candidate,
    ));
    const host = fixture.socket([], CLEANUP_DIGEST, GRANT_A);
    await fixture.broker.message(host.webSocket, JSON.stringify({
      type: "catalog",
      tools: [cleanupEntry()],
    }));
    expect(host.closed).toBeUndefined();
    const selected = fixture.broker.provider().resolve("cleanup")!;
    expect(selected).toBeDefined();

    digest = undefined;
    expect(fixture.broker.provider().resolve("cleanup")).toBeUndefined();
    await expect(selected.handler({}, { sessionId: "session:1", callId: "source:1" }))
      .resolves.toMatchObject({
        success: false,
        structuredResult: { status: "unavailable" },
      });
    expect(host.sent.some((frame) => frame.type === "call")).toBe(false);
  });

  it("projects a retained catalog and blocks a stale tool when the active grant changes", async () => {
    let allowed = true;
    const fixture = createFixture((candidate) => allowed && candidate.provider === "fixture");
    const host = fixture.socket();
    await catalog(fixture.broker, host);
    const selected = fixture.broker.provider().resolve("fixture__lookup")!;
    expect(fixture.broker.provider().definitions()).toHaveLength(1);

    allowed = false;
    expect(fixture.broker.provider().definitions()).toEqual([]);
    expect(fixture.broker.provider().resolve("fixture__lookup")).toBeUndefined();
    await expect(selected.handler({}, { sessionId: "session:1", callId: "source:1" }))
      .resolves.toMatchObject({
        success: false,
        structuredResult: { status: "unavailable" },
      });
    expect(host.sent.some((frame) => frame.type === "call")).toBe(false);
  });

  it("marks dispatched calls ambiguous after unexpected transport loss", async () => {
    const fixture = createFixture();
    const host = fixture.socket();
    await catalog(fixture.broker, host);
    const tool = fixture.broker.provider().resolve("fixture__lookup")!;
    const pending = tool.handler({}, { sessionId: "session:1", callId: "source:1" });
    fixture.broker.webSocketClose(host.webSocket, 1006, "network lost");
    await expect(pending).resolves.toMatchObject({
      success: false,
      structuredResult: { status: "ambiguous" },
    });
    expect(fixture.persistence.call(IDS[1]!)?.state).toBe("ambiguous");
  });

  it("preserves pre-admission fallback and does not reroute a selected stale binding", async () => {
    const fixture = createFixture();
    const first = fixture.socket();
    await catalog(fixture.broker, first);
    const selected = fixture.broker.provider().resolve("fixture__lookup")!;
    const replacement = fixture.socket();
    await catalog(fixture.broker, replacement);
    const outcome = await selected.handler({}, { sessionId: "session:1", callId: "source:1" });
    expect((outcome as Record<PropertyKey, unknown>)[HOSTED_TOOLS_PRE_ADMISSION_UNAVAILABLE]).toBe(true);
    expect(replacement.sent.some((frame) => frame.type === "call")).toBe(false);
  });

  it("sends best-effort cancellation and accepts cancellation as the ordinary result outcome", async () => {
    const fixture = createFixture();
    const host = fixture.socket();
    await catalog(fixture.broker, host);
    const controller = new AbortController();
    const pending = fixture.broker.provider().resolve("fixture__lookup")!.handler({}, {
      sessionId: "session:1",
      callId: "source:1",
      signal: controller.signal,
    });
    controller.abort();
    expect(host.sent.at(-1)).toEqual({ type: "cancel", call_id: IDS[1] });
    await fixture.broker.message(host.webSocket, JSON.stringify({
      type: "result",
      call_id: IDS[1],
      outcome: { status: "cancelled", message: "cancelled by executor" },
    }));
    await expect(pending).resolves.toMatchObject({
      success: false,
      structuredResult: { status: "cancelled" },
    });
    expect(host.sent.at(-1)).toEqual({ type: "ack", call_id: IDS[1] });
  });

  it("uses close code and reason as the only protocol rejection", async () => {
    const fixture = createFixture();
    const host = fixture.socket();
    await fixture.broker.message(host.webSocket, JSON.stringify({ type: "attach", protocol_version: 1 }));
    expect(host.closed).toMatchObject({ code: 1008, reason: expect.stringContaining("unknown_message") });
    expect(host.sent).toEqual([]);
  });
});

function createFixture(
  entryAllowed?: (
    entry: HostedToolCatalogEntry,
    connectGrantId?: string,
    appToolCatalogDigest?: string,
  ) => boolean,
) {
  const persistence = new MemoryPersistence();
  const sockets: FakeSocket[] = [];
  const ids = [...IDS];
  const context = {
    storage: {} as DurableObjectStorage,
    acceptWebSocket() {},
    getWebSockets: () => sockets.map((socket) => socket.webSocket),
  } as unknown as HostedToolsBrokerContext;
  const broker = new HostedToolsBroker(context, {
    persistence,
    now: () => NOW,
    ...(entryAllowed === undefined ? {} : { entryAllowed }),
    randomUUID: () => ids.shift() ?? crypto.randomUUID(),
  });
  return {
    broker,
    persistence,
    socket(
      allowedMcpIds?: readonly string[],
      appToolCatalogDigest?: `0x${string}`,
      connectGrantId?: string,
    ) {
      const socket = new FakeSocket();
      socket.serializeAttachment({
        kind: "hosted-tools",
        sessionId: "session:route",
        ...(allowedMcpIds === undefined ? {} : { allowedMcpIds }),
        ...(appToolCatalogDigest === undefined ? {} : { appToolCatalogDigest }),
        ...(connectGrantId === undefined ? {} : { connectGrantId }),
      });
      sockets.push(socket);
      return socket;
    },
  };
}

class FakeSocket {
  readonly sent: Record<string, unknown>[] = [];
  closed?: { code: number; reason: string };
  onSend?: (frame: Record<string, unknown>) => void;
  #attachment: unknown;
  readyState = WebSocket.OPEN;
  readonly webSocket = this as unknown as WebSocket;

  serializeAttachment(value: unknown): void { this.#attachment = structuredClone(value); }
  deserializeAttachment(): unknown { return structuredClone(this.#attachment); }
  send(encoded: string): void {
    const frame = JSON.parse(encoded) as Record<string, unknown>;
    this.onSend?.(frame);
    this.sent.push(frame);
  }
  close(code: number, reason: string): void {
    this.readyState = WebSocket.CLOSED;
    this.closed = { code, reason };
  }
}

class MemoryPersistence implements HostedToolsBrokerPersistence {
  current: State = {
    generation: 0,
    host_id: null,
    lease_id: null,
    lease_expires_at: 0,
    catalog_json: null,
  };
  readonly calls = new Map<string, CallRow>();

  initialize(_now: number): State | undefined { return undefined; }
  transaction<T>(callback: () => T): T { return callback(); }
  state(): State { return structuredClone(this.current); }
  replaceHost(row: State): void { this.current = structuredClone(row); }
  clearHost(leaseId: string, generation: number): void {
    if (this.current.lease_id !== leaseId || this.current.generation !== generation) return;
    this.current = {
      ...this.current,
      host_id: null,
      lease_id: null,
      lease_expires_at: 0,
      catalog_json: null,
    };
  }
  clearCatalog(leaseId: string, generation: number): void {
    if (this.current.lease_id !== leaseId || this.current.generation !== generation) return;
    this.current = {
      ...this.current,
      catalog_json: null,
    };
  }
  call(callId: string): CallRow | undefined {
    const row = this.calls.get(callId);
    return row && structuredClone(row);
  }
  callBySource(sessionId: string, sourceCallId: string): CallRow | undefined {
    const row = [...this.calls.values()].find((candidate) => candidate.session_id === sessionId
      && candidate.source_call_id === sourceCallId);
    return row && structuredClone(row);
  }
  insertCall(row: CallRow): void {
    if (this.calls.has(row.call_id) || this.callBySource(row.session_id, row.source_call_id)) {
      throw new Error("duplicate call");
    }
    this.calls.set(row.call_id, structuredClone(row));
  }
  markCancelRequested(callId: string): CallRow | undefined {
    const row = this.calls.get(callId);
    if (row?.state === "dispatched") row.cancel_requested = 1;
    return this.call(callId);
  }
  transitionCall(
    callId: string,
    from: readonly CallState[],
    state: CallState,
    resultJson: string,
  ): CallRow | undefined {
    const row = this.calls.get(callId);
    if (row && from.includes(row.state)) {
      row.state = state;
      row.result_json = resultJson || null;
    }
    return this.call(callId);
  }
  recordLateReceipt(callId: string, receiptJson: string): CallRow | undefined {
    const row = this.calls.get(callId);
    if (row?.state === "ambiguous" && row.receipt_json === null) row.receipt_json = receiptJson;
    return this.call(callId);
  }
  markGenerationAmbiguous(leaseId: string, generation: number, resultJson: string): void {
    for (const row of this.calls.values()) {
      if (row.lease_id === leaseId && row.generation === generation && row.state === "dispatched") {
        row.state = "ambiguous";
        row.result_json = resultJson;
      }
    }
  }
  activeCallCount(leaseId: string, generation: number): number {
    return [...this.calls.values()].filter((row) => row.lease_id === leaseId
      && row.generation === generation
      && (row.state === "admitted" || row.state === "dispatched")).length;
  }
  generationCallCount(leaseId: string, generation: number): number {
    return [...this.calls.values()].filter((row) => row.lease_id === leaseId
      && row.generation === generation).length;
  }
  pruneReceipts(_activeLeaseId: string | null, _activeGeneration: number, _limit: number): void {}
}

async function catalog(broker: HostedToolsBroker, host: FakeSocket): Promise<void> {
  await broker.message(host.webSocket, JSON.stringify({ type: "catalog", tools: [entry()] }));
}

function entry() {
  return {
    provider: "fixture",
    remote_name: "lookup",
    definition: {
      type: "function" as const,
      name: "fixture__lookup",
      description: "Look up one fixture",
      strict: true,
      parameters: { type: "object", properties: {} },
    },
    parallel_safe: true,
    summary: "Fixture lookup",
    timeout_ms: 30_000,
  };
}

function cleanupEntry(remoteName = "cleanup", strict = false): HostedToolCatalogEntry {
  return {
    provider: "javascript",
    remote_name: remoteName,
    definition: {
      type: "function",
      name: "cleanup",
      description: "List open web tabs, inspect one exact tab, and preview or revert one declarative CSS cleanup recipe.",
      strict,
      parameters: {
        oneOf: [
          {
            type: "object",
            properties: {
              action: { const: "list_tabs" },
              cursor: { type: "string", minLength: 1, maxLength: 80 },
            },
            required: ["action"],
            additionalProperties: false,
          },
          {
            type: "object",
            properties: {
              action: { const: "inspect" },
              tab_ref: { type: "string", minLength: 1, maxLength: 80 },
            },
            required: ["action"],
            additionalProperties: false,
          },
          {
            type: "object",
            properties: {
              action: { const: "preview" },
              document_revision: { type: "string" },
              recipe: {
                type: "object",
                properties: {
                  schema_version: { const: 1 },
                  name: { type: "string", minLength: 1, maxLength: 80 },
                  css: { type: "string", maxLength: 32768 },
                  hide_selectors: {
                    type: "array",
                    maxItems: 64,
                    items: { type: "string", minLength: 1, maxLength: 512 },
                  },
                },
                required: ["name", "css", "hide_selectors"],
                additionalProperties: false,
              },
            },
            required: ["action", "document_revision", "recipe"],
            additionalProperties: false,
          },
          {
            type: "object",
            properties: {
              action: { const: "revert_preview" },
              preview_id: { type: "string" },
            },
            required: ["action", "preview_id"],
            additionalProperties: false,
          },
        ],
      },
    },
    parallel_safe: false,
    timeout_ms: 120_000,
  };
}

function result(callId: string, output: string): string {
  return JSON.stringify({
    type: "result",
    call_id: callId,
    outcome: {
      status: "completed",
      output: {
        output,
        success: true,
        structured_result: { output },
        metadata: null,
        process_trace: null,
      },
    },
  });
}
