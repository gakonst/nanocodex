import { describe, expect, it } from "vitest";
import {
  MAX_HOSTED_TOOLS_FRAME_BYTES,
  HostedToolsProtocolError,
  parseHostedToolsHostFrame,
  parseHostedToolsManagedFrame,
  type HostedToolCatalogEntry,
} from "../src/hosted-tools-protocol";

const tool: HostedToolCatalogEntry = {
  provider: "local",
  remote_name: "lookup",
  definition: {
    type: "function",
    name: "lookup",
    description: "Looks up a value",
    strict: true,
    parameters: { type: "object", properties: { key: { type: "string" } }, required: ["key"] },
    output_schema: { type: "object" },
  },
  parallel_safe: true,
  summary: "Lookup",
  timeout_ms: 1_000,
};

const outcome = {
  status: "completed" as const,
  output: {
    output: "ok",
    success: true,
    structured_result: { ok: true },
    metadata: { source: "local" },
    process_trace: null,
  },
};

describe("hosted tools socket protocol", () => {
  it("parses the exact executor-to-DO frame set", () => {
    const machines = [{
      id: "laptop",
      name: "George's laptop",
      workspace: "/Users/george/project",
      capabilities: ["filesystem", "native-shell"],
    }];
    expect(parseHostedToolsHostFrame(JSON.stringify({
      type: "catalog",
      tools: [tool],
      machines,
      attachment_id: "laptop",
    }))).toEqual({ type: "catalog", tools: [tool], machines, attachment_id: "laptop" });
    expect(parseHostedToolsHostFrame(JSON.stringify({ type: "catalog", tools: [tool] })))
      .toEqual({ type: "catalog", tools: [tool] });
    const maximumAttachmentId = "a".repeat(123);
    expect(parseHostedToolsHostFrame(JSON.stringify({
      type: "catalog",
      tools: [tool],
      attachment_id: maximumAttachmentId,
    }))).toEqual({ type: "catalog", tools: [tool], attachment_id: maximumAttachmentId });
    expect(parseHostedToolsHostFrame(JSON.stringify({ type: "result", call_id: "call:1", outcome })))
      .toEqual({ type: "result", call_id: "call:1", outcome });
    expect(parseHostedToolsHostFrame(JSON.stringify({ type: "ping", nonce: "n-1" })))
      .toEqual({ type: "ping", nonce: "n-1" });
    expect(parseHostedToolsHostFrame(JSON.stringify({ type: "drain" }))).toEqual({ type: "drain" });
  });

  it("accepts a bounded oneOf of object inputs and rejects non-object branches", () => {
    const oneOfTool = {
      ...tool,
      definition: {
        ...tool.definition,
        parameters: {
          oneOf: [
            { type: "object", properties: { action: { const: "inspect" } } },
            { type: "object", properties: { action: { const: "preview" } } },
          ],
        },
      },
    };
    expect(parseHostedToolsHostFrame(JSON.stringify({ type: "catalog", tools: [oneOfTool] })))
      .toMatchObject({ type: "catalog" });
    expect(() => parseHostedToolsHostFrame(JSON.stringify({
      type: "catalog",
      tools: [{
        ...oneOfTool,
        definition: {
          ...oneOfTool.definition,
          parameters: { oneOf: [{ type: "object" }, { type: "string" }] },
        },
      }],
    }))).toThrow("object JSON Schema");
  });

  it("parses the exact DO-to-executor frame set", () => {
    expect(parseHostedToolsManagedFrame(JSON.stringify({ type: "ready" }))).toEqual({ type: "ready" });
    expect(parseHostedToolsManagedFrame(JSON.stringify({
      type: "call",
      session_id: "session:1",
      call_id: "call:1",
      model: "gpt-5.2",
      name: "lookup",
      input: { key: "a" },
      output_token_budget: 100,
      output_byte_budget: 1_024,
      deadline_at: 1_800_000_000_000,
    }))).toMatchObject({ type: "call", call_id: "call:1", name: "lookup" });
    expect(parseHostedToolsManagedFrame(JSON.stringify({ type: "cancel", call_id: "call:1" })))
      .toEqual({ type: "cancel", call_id: "call:1" });
    expect(parseHostedToolsManagedFrame(JSON.stringify({ type: "ack", call_id: "call:1" })))
      .toEqual({ type: "ack", call_id: "call:1" });
    expect(parseHostedToolsManagedFrame(JSON.stringify({ type: "pong", nonce: "n-1" })))
      .toEqual({ type: "pong", nonce: "n-1" });
    expect(parseHostedToolsManagedFrame(JSON.stringify({ type: "draining" }))).toEqual({ type: "draining" });
  });

  it("rejects legacy pins, removed frames, wrong directions, and extra fields", () => {
    for (const frame of [
      { type: "catalog", tools: [tool], protocol_version: 1 },
      { type: "catalog", tools: [tool], capability: "tools" },
      { type: "catalog", tools: [tool], host_id: "host" },
      { type: "catalog", tools: [tool], lease_id: "lease" },
      { type: "catalog", tools: [tool], catalog_revision: 1 },
      { type: "catalog", tools: [tool], catalog_digest: "0".repeat(64) },
      { type: "fenced" },
      { type: "cancel_ack", call_id: "call:1" },
      { type: "result_ack", call_id: "call:1" },
    ]) expect(() => parseHostedToolsHostFrame(JSON.stringify(frame))).toThrow(HostedToolsProtocolError);
    expect(() => parseHostedToolsHostFrame(JSON.stringify({ type: "ready" }))).toThrow("host-to-managed");
    expect(() => parseHostedToolsManagedFrame(JSON.stringify({ type: "drain" }))).toThrow("managed-to-host");
  });

  it("preserves catalog and output validation", () => {
    expect(() => parseHostedToolsHostFrame(JSON.stringify({ type: "catalog", tools: [tool, tool] })))
      .toThrow("duplicate tool name");
    expect(() => parseHostedToolsHostFrame(JSON.stringify({
      type: "result",
      call_id: "call:1",
      outcome: { status: "completed", output: { output: "ok", success: true } },
    }))).toThrow("nullable output metadata fields");
    expect(() => parseHostedToolsHostFrame("{" )).toThrow("JSON objects");
    expect(() => parseHostedToolsHostFrame(JSON.stringify({ type: "ping", nonce: "x".repeat(129) })))
      .toThrow("nonce");
    expect(() => parseHostedToolsHostFrame("x".repeat(MAX_HOSTED_TOOLS_FRAME_BYTES + 1)))
      .toThrow("limited");
    expect(() => parseHostedToolsHostFrame(JSON.stringify({
      type: "catalog",
      tools: [tool],
      machines: [{
        id: "laptop",
        name: "Laptop",
        workspace: "/workspace",
        capabilities: [],
        token: "secret",
      }],
    }))).toThrow("unsupported field token");
    const machine = {
      id: "laptop",
      name: "Laptop",
      workspace: "/workspace",
      capabilities: ["filesystem"],
    };
    expect(() => parseHostedToolsHostFrame(JSON.stringify({
      type: "catalog",
      tools: [tool],
      machines: Array.from({ length: 2 }, (_, index) => ({
        ...machine,
        id: `machine:${index}`,
      })),
    }))).toThrow("at most 1");
    for (const attachmentId of ["", "unsafe id", "é", "x".repeat(124), 1, null]) {
      expect(() => parseHostedToolsHostFrame(JSON.stringify({
        type: "catalog",
        tools: [tool],
        attachment_id: attachmentId,
      }))).toThrow("attachment_id must be 1-123 safe ASCII bytes");
    }
    expect(() => parseHostedToolsHostFrame(JSON.stringify({
      type: "catalog",
      tools: [tool],
      machines: [machine],
      attachment_id: "desktop",
    }))).toThrow("id equals attachment_id");
    expect(() => parseHostedToolsHostFrame(JSON.stringify({
      type: "catalog",
      tools: [tool],
      machines: [machine],
    }))).toThrow("id equals attachment_id");
  });
});

it("validates observation direction, metadata and image bounds", () => {
  const observe = { type: "observe", request_id: "request:1", surface_id: "screen" };
  expect(parseHostedToolsManagedFrame(JSON.stringify(observe))).toEqual(observe);
  expect(() => parseHostedToolsHostFrame(JSON.stringify(observe))).toThrow();
  expect(() => parseHostedToolsManagedFrame(JSON.stringify({ ...observe, surface_id: "bad/id" }))).toThrow();
  expect(() => parseHostedToolsHostFrame(JSON.stringify({ type: "catalog", tools: [], observation_surfaces: [{ id: "screen", name: "Screen", kind: "desktop" }] }))).toThrow();
  const observation = { type: "observation", request_id: "request:1", result: { status: "frame", frame: { captured_at: 1, width: 1, height: 1, mime_type: "image/jpeg", data: "AAAA" } } };
  expect(parseHostedToolsHostFrame(JSON.stringify(observation))).toEqual(observation);
  expect(() => parseHostedToolsManagedFrame(JSON.stringify(observation))).toThrow();
  observation.result.frame.data = "a".repeat(240004);
  expect(() => parseHostedToolsHostFrame(JSON.stringify(observation))).toThrow();
});
