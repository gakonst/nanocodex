import { describe, expect, it, vi } from "vitest";
import { hostedToolCatalogDigest } from "nanocodex/tools/hosted-catalog";
import {
  EXEC_COMMAND_PARAMETERS,
  EXECUTION_OUTPUT_SCHEMA,
  MACHINE_PREVIEW_PARAMETERS,
  PREVIEW_OUTPUT_SCHEMA,
  WRITE_STDIN_PARAMETERS,
} from "nanocodex-tools/execution-contract";

import {
  HostedToolsBroker,
  HOSTED_TOOLS_PRE_ADMISSION_UNAVAILABLE,
  type HostedToolsAuthorizationContext,
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
  "33333333-3333-4333-8333-333333333333",
  "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
  "44444444-4444-4444-8444-444444444444",
  "dddddddd-dddd-4ddd-8ddd-dddddddddddd",
];
const CLEANUP_DIGEST = await hostedToolCatalogDigest([cleanupEntry()]);

type State = NonNullable<ReturnType<HostedToolsBrokerPersistence["state"]>>;
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
      attachment_id: "desktop",
      tools: [entry()],
      machines,
    }));

    expect(fixture.broker.machines()).toEqual(machines);
    await fixture.broker.message(host.webSocket, JSON.stringify({ type: "drain" }));
    expect(fixture.broker.machines()).toEqual([]);
  });

  it("caps leased attachments at their control lease and revokes their exact route", async () => {
    const fixture = createFixture();
    const firstRoute = "vm-host:33333333-3333-4333-8333-333333333333:1";
    const successorRoute = "vm-host:33333333-3333-4333-8333-333333333333:2";
    const host = fixture.socket(undefined, undefined, undefined, "leased-vm", NOW + 10, firstRoute);
    await fixture.broker.message(host.webSocket, JSON.stringify({
      type: "catalog",
      attachment_id: "leased-vm",
      tools: [machineEntry("exec_command")],
      machines: [{
        id: "leased-vm",
        name: "Leased VM",
        workspace: "/workspace",
        capabilities: ["filesystem"],
      }],
    }));

    expect(fixture.persistence.state(firstRoute)?.lease_expires_at).toBe(NOW + 10);
    await fixture.broker.message(host.webSocket, JSON.stringify({ type: "ping", nonce: "" }));
    expect(fixture.persistence.state(firstRoute)?.lease_expires_at).toBe(NOW + 10);

    fixture.persistence.routes.get(firstRoute)!.lease_expires_at = NOW - 1;
    fixture.broker.expire();
    expect(host.closed).toMatchObject({ code: 1008 });

    const successor = fixture.socket(
      undefined, undefined, undefined, "leased-vm", NOW + 20, successorRoute,
    );
    await fixture.broker.message(successor.webSocket, JSON.stringify({
      type: "catalog",
      attachment_id: "leased-vm",
      tools: [machineEntry("exec_command")],
      machines: [{
        id: "leased-vm",
        name: "Successor VM",
        workspace: "/workspace",
        capabilities: ["filesystem"],
      }],
    }));

    expect(fixture.broker.revokeRoute(firstRoute, "delayed old revocation")).toBe(false);
    expect(successor.closed).toBeUndefined();
    expect(fixture.broker.machines()).toEqual([expect.objectContaining({ id: "leased-vm" })]);
    expect(fixture.broker.revokeRoute(successorRoute, "current control lease ended")).toBe(true);
    expect(successor.closed).toMatchObject({ code: 1008 });
  });

  it("revalidates a leased bearer and keeps the exact route live past its initial control lease", async () => {
    let now = NOW;
    const route = "vm-host:33333333-3333-4333-8333-333333333333:4";
    const renew = vi.fn(async () => now + 60_000);
    const fixture = createFixture(undefined, {
      now: () => now,
      renewLeasedAttachment: renew,
    });
    const host = fixture.socket(
      undefined, undefined, undefined, "leased-vm", NOW + 60_000, route, "opaque-renewal",
    );
    await fixture.broker.message(host.webSocket, JSON.stringify({
      type: "catalog",
      attachment_id: "leased-vm",
      tools: [machineEntry("exec_command")],
      machines: [{
        id: "leased-vm",
        name: "Leased VM",
        workspace: "/workspace",
        capabilities: ["filesystem"],
      }],
    }));

    now += 40_000;
    await fixture.broker.message(host.webSocket, JSON.stringify({ type: "ping", nonce: "renew" }));
    expect(renew).toHaveBeenCalledWith({
      expectedAttachmentId: "leased-vm",
      fixedRouteId: route,
      renewalToken: "opaque-renewal",
    });
    expect(fixture.persistence.state(route)?.lease_expires_at).toBe(NOW + 100_000);

    now = NOW + 61_000;
    fixture.broker.expire();
    expect(host.closed).toBeUndefined();
    expect(fixture.broker.machineOnRoute(route, "leased-vm")).toBeDefined();

    renew.mockResolvedValueOnce(undefined as never);
    await fixture.broker.message(host.webSocket, JSON.stringify({ type: "ping", nonce: "stale" }));
    expect(host.closed).toMatchObject({ code: 1008 });
    expect(fixture.broker.machineOnRoute(route, "leased-vm")).toBeUndefined();
  });

  it("durably rejects a leased route revoked before catalog admission", async () => {
    const fixture = createFixture();
    const route = "vm-host:44444444-4444-4444-8444-444444444444:7";
    expect(fixture.broker.revokeRoute(route, "control lease already ended")).toBe(false);

    const delayed = fixture.socket(undefined, undefined, undefined, "leased-vm", NOW + 20, route);
    await fixture.broker.message(delayed.webSocket, JSON.stringify({
      type: "catalog",
      attachment_id: "leased-vm",
      tools: [machineEntry("exec_command")],
      machines: [{
        id: "leased-vm",
        name: "Delayed stale VM",
        workspace: "/workspace",
        capabilities: ["filesystem"],
      }],
    }));

    expect(delayed.closed).toMatchObject({
      code: 1008,
      reason: expect.stringContaining("route_revoked"),
    });
    expect(fixture.broker.machines()).toEqual([]);

    const resumed = new HostedToolsBroker(fixture.context, {
      persistence: fixture.persistence,
      now: () => NOW,
      resumeRetainedSockets: true,
    });
    const retried = fixture.socket(undefined, undefined, undefined, "leased-vm", NOW + 30, route);
    await resumed.message(retried.webSocket, JSON.stringify({
      type: "catalog",
      attachment_id: "leased-vm",
      tools: [machineEntry("exec_command")],
      machines: [{
        id: "leased-vm",
        name: "Retried stale VM",
        workspace: "/workspace",
        capabilities: ["filesystem"],
      }],
    }));
    expect(retried.closed?.reason).toContain("route_revoked");
  });

  it("resolves leased machines and tools only through their exact live route", async () => {
    const fixture = createFixture();
    const route = "vm-host:44444444-4444-4444-8444-444444444444:8";
    const leased = fixture.socket(undefined, undefined, undefined, "vm:mount", NOW + 20, route);
    await fixture.broker.message(leased.webSocket, JSON.stringify({
      type: "catalog",
      attachment_id: "vm:mount",
      tools: [machineEntry("exec_command")],
      machines: [{
        id: "vm:mount",
        name: "Leased VM",
        workspace: "/workspace",
        capabilities: ["filesystem"],
      }],
    }));

    expect(fixture.broker.machineOnRoute(route, "vm:mount")).toMatchObject({ id: "vm:mount" });
    expect(fixture.broker.machineToolOnRoute(route, "vm:mount", "exec_command")).toBeDefined();
    fixture.broker.revokeRoute(route, "lease replaced");

    const ordinary = fixture.socket();
    await fixture.broker.message(ordinary.webSocket, JSON.stringify({
      type: "catalog",
      attachment_id: "vm:mount",
      tools: [machineEntry("exec_command")],
      machines: [{
        id: "vm:mount",
        name: "Impostor",
        workspace: "/tmp/impostor",
        capabilities: ["filesystem"],
      }],
    }));

    expect(fixture.broker.machines()).toEqual([expect.objectContaining({ name: "Impostor" })]);
    expect(fixture.broker.machineOnRoute(route, "vm:mount")).toBeUndefined();
    expect(fixture.broker.machineToolOnRoute(route, "vm:mount", "exec_command")).toBeUndefined();
  });

  it.each([
    ["after its root turn ends", undefined],
    ["while a differently authorized root is active", "connect"],
  ] as const)("uses retained child authority for a leased VM %s", async (_label, rootAuthority) => {
    const childSessionId = "01995555-5555-7555-8555-555555555555";
    const fixture = createFixture((_entry, _grantId, _digest, context) => {
      const authority = context?.subagent?.sessionId === childSessionId
        ? "account"
        : rootAuthority;
      return authority === "account";
    });
    const route = "vm-host:44444444-4444-4444-8444-444444444444:10";
    const leased = fixture.socket(undefined, undefined, undefined, "vm:retained", NOW + 20, route);
    await fixture.broker.message(leased.webSocket, JSON.stringify({
      type: "catalog",
      attachment_id: "vm:retained",
      tools: [machineEntry("exec_command")],
      machines: [{
        id: "vm:retained",
        name: "Retained child VM",
        workspace: "/workspace",
        capabilities: ["filesystem"],
      }],
    }));
    const context = {
      sessionId: childSessionId,
      callId: `source:${rootAuthority ?? "ended"}`,
      subagent: {
        agentId: "child-agent",
        parentAgentId: null,
        sessionId: childSessionId,
        role: "worker",
        task: "continue after the root turn",
      },
    };

    expect(fixture.broker.machineToolOnRoute(
      route,
      "vm:retained",
      "exec_command",
    )).toBeUndefined();
    const selected = fixture.broker.machineToolOnRoute(
      route,
      "vm:retained",
      "exec_command",
      context,
    );
    expect(selected).toBeDefined();
    const pending = selected!.handler({ cmd: "pwd" }, context);
    const call = leased.sent.find((frame) => frame.type === "call")!;
    await fixture.broker.message(leased.webSocket, result(call.call_id as string, "retained"));
    await expect(pending).resolves.toMatchObject({ output: "retained" });
  });

  it("rejects non-machine tools from leased attachments", async () => {
    const fixture = createFixture();
    const route = "vm-host:44444444-4444-4444-8444-444444444444:9";
    const leased = fixture.socket(undefined, undefined, undefined, "vm:mount", NOW + 20, route);
    await fixture.broker.message(leased.webSocket, JSON.stringify({
      type: "catalog",
      attachment_id: "vm:mount",
      tools: [machineEntry("exec_command"), entry("fixture__injected")],
      machines: [{
        id: "vm:mount",
        name: "Leased VM",
        workspace: "/workspace",
        capabilities: ["filesystem"],
      }],
    }));

    expect(leased.closed).toMatchObject({
      code: 1008,
      reason: expect.stringContaining("leased tool attachments may publi"),
    });
    expect(fixture.broker.provider().definitions()).toEqual([]);
  });

  it("replaces the live machine snapshot without rebuilding its broker", async () => {
    const fixture = createFixture();
    const first = fixture.socket();
    await fixture.broker.message(first.webSocket, JSON.stringify({
      type: "catalog",
      attachment_id: "laptop",
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
      attachment_id: "laptop",
      tools: [entry()],
      machines: [{
        id: "laptop",
        name: "Renamed laptop",
        workspace: "/home/george/repo",
        capabilities: ["filesystem", "native-shell"],
      }],
    }));

    expect(first.closed).toMatchObject({ code: 1008 });
    expect(fixture.broker.machines()).toEqual([{
      id: "laptop",
      name: "Renamed laptop",
      workspace: "/home/george/repo",
      capabilities: ["filesystem", "native-shell"],
    }]);
  });

  it("reserves canonical machine tools, resolves exact machines, and disconnects independently", async () => {
    const fixture = createFixture();
    const routeB = fixture.socket();
    await fixture.broker.message(routeB.webSocket, JSON.stringify({
      type: "catalog",
      attachment_id: "machine-b",
      tools: [machineEntry("exec_command")],
      machines: [{ id: "machine-b", name: "Machine B", workspace: "/b", capabilities: ["shell"] }],
    }));
    const routeA = fixture.socket();
    await fixture.broker.message(routeA.webSocket, JSON.stringify({
      type: "catalog",
      attachment_id: "machine-a",
      tools: [machineEntry("exec_command")],
      machines: [{ id: "machine-a", name: "Machine A", workspace: "/a", capabilities: ["filesystem"] }],
    }));

    expect(fixture.broker.provider().definitions().map((definition) => definition.name))
      .toEqual([]);
    expect(fixture.broker.provider().resolve("user_machine-a_exec_command")).toBeUndefined();
    expect(fixture.broker.machines().map((machine) => machine.id)).toEqual(["machine-a", "machine-b"]);

    const pendingA = fixture.broker.machineTool("machine-a", "exec_command")!.handler({ cmd: "pwd" }, {
      sessionId: "session:1", callId: "source:a",
    });
    const pendingB = fixture.broker.machineTool("machine-b", "exec_command")!.handler({ cmd: "pwd" }, {
      sessionId: "session:1", callId: "source:b",
    });
    const callA = routeA.sent.find((frame) => frame.type === "call")!;
    const callB = routeB.sent.find((frame) => frame.type === "call")!;
    expect(callA.name).toBe("exec_command");
    expect(callB.name).toBe("exec_command");
    await fixture.broker.message(routeA.webSocket, result(callA.call_id as string, "from A"));
    await fixture.broker.message(routeB.webSocket, result(callB.call_id as string, "from B"));
    await expect(pendingA).resolves.toMatchObject({
      output: "from A",
      metadata: {
        machine_id: "machine-a",
        machine_name: "Machine A",
        tool_name: "exec_command",
      },
    });
    await expect(pendingB).resolves.toMatchObject({
      output: "from B",
      metadata: {
        machine_id: "machine-b",
        machine_name: "Machine B",
        tool_name: "exec_command",
      },
    });

    const replacementA = fixture.socket();
    await fixture.broker.message(replacementA.webSocket, JSON.stringify({
      type: "catalog",
      attachment_id: "machine-a",
      tools: [machineEntry("exec_command")],
      machines: [{ id: "machine-a", name: "Machine A2", workspace: "/a2", capabilities: ["filesystem"] }],
    }));
    expect(routeA.closed).toMatchObject({ code: 1008 });
    expect(routeB.closed).toBeUndefined();
    expect(fixture.broker.provider().definitions().map((definition) => definition.name))
      .toEqual([]);

    fixture.persistence.routes.get("user:machine-a")!.lease_expires_at = NOW + 1;
    const routeBExpiry = fixture.persistence.routes.get("user:machine-b")!.lease_expires_at;
    await fixture.broker.message(replacementA.webSocket, JSON.stringify({ type: "ping", nonce: "alive" }));
    expect(replacementA.sent.at(-1)).toEqual({ type: "pong", nonce: "alive" });
    expect(fixture.persistence.routes.get("user:machine-a")!.lease_expires_at).toBeGreaterThan(NOW + 1);
    expect(fixture.persistence.routes.get("user:machine-b")!.lease_expires_at).toBe(routeBExpiry);

    await fixture.broker.message(replacementA.webSocket, JSON.stringify({ type: "drain" }));
    expect(fixture.broker.machineTool("machine-a", "exec_command")).toBeUndefined();
    expect(fixture.broker.machineTool("machine-b", "exec_command")).toBeDefined();
    expect(fixture.broker.machines().map((machine) => machine.id)).toEqual(["machine-b"]);
    fixture.broker.webSocketClose(replacementA.webSocket, 1000, "done");
    expect(fixture.broker.machineTool("machine-b", "exec_command")).toBeDefined();

    fixture.broker.webSocketClose(routeB.webSocket, 1000, "done");
    expect(fixture.broker.provider().definitions()).toEqual([]);
    expect(fixture.broker.machines()).toEqual([]);
  });

  it("keeps a resolved canonical machine tool pinned to its admitted generation", async () => {
    const fixture = createFixture();
    const first = fixture.socket();
    await fixture.broker.message(first.webSocket, JSON.stringify({
      type: "catalog",
      attachment_id: "machine-a",
      tools: [machineEntry("exec_command")],
      machines: [{ id: "machine-a", name: "Machine A", workspace: "/a", capabilities: ["shell"] }],
    }));
    const selected = fixture.broker.machineTool("machine-a", "exec_command")!;

    const replacement = fixture.socket();
    await fixture.broker.message(replacement.webSocket, JSON.stringify({
      type: "catalog",
      attachment_id: "machine-a",
      tools: [machineEntry("exec_command")],
      machines: [{ id: "machine-a", name: "Machine A2", workspace: "/a2", capabilities: ["shell"] }],
    }));

    const outcome = await selected.handler({ cmd: "pwd" }, {
      sessionId: "session:1", callId: "source:stale",
    });
    expect((outcome as Record<PropertyKey, unknown>)[HOSTED_TOOLS_PRE_ADMISSION_UNAVAILABLE]).toBe(true);
    expect(replacement.sent.some((frame) => frame.type === "call")).toBe(false);
    expect(fixture.persistence.callBySource("session:1", "source:stale")).toBeUndefined();
  });

  it("admits only canonical selector-free machine primitive schemas", async () => {
    const fixture = createFixture();
    const host = fixture.socket();
    const tools = [
      machineEntry("exec_command"),
      machineEntry("write_stdin"),
      machineEntry("preview"),
    ];
    await fixture.broker.message(host.webSocket, JSON.stringify({
      type: "catalog",
      attachment_id: "machine-a",
      tools,
      machines: [{ id: "machine-a", name: "Machine A", workspace: "/a", capabilities: ["shell"] }],
    }));

    expect(host.closed).toBeUndefined();
    expect(fixture.broker.provider().definitions()).toEqual([]);
    for (const tool of tools) {
      expect(tool.definition.parameters.properties).not.toHaveProperty("environment");
      expect(tool.definition.parameters.properties).not.toHaveProperty("machine");
      expect(tool.definition.parameters.properties).not.toHaveProperty("host");
      expect(fixture.broker.machineTool("machine-a", tool.definition.name)).toBeDefined();
    }

    const invalid = createFixture();
    const invalidHost = invalid.socket();
    const invalidExec = machineEntry("exec_command");
    (invalidExec.definition.parameters.properties as Record<string, unknown>).environment = { type: "string" };
    await invalid.broker.message(invalidHost.webSocket, JSON.stringify({
      type: "catalog",
      attachment_id: "machine-b",
      tools: [invalidExec],
      machines: [{ id: "machine-b", name: "Machine B", workspace: "/b", capabilities: ["shell"] }],
    }));
    expect(invalidHost.closed).toMatchObject({
      code: 1008,
      reason: expect.stringContaining("machine tool exec_command"),
    });
  });

  it("expires only the named route whose lease elapsed", async () => {
    const fixture = createFixture();
    const routeA = fixture.socket();
    await fixture.broker.message(routeA.webSocket, JSON.stringify({
      type: "catalog",
      attachment_id: "machine-a",
      tools: [entry("alpha")],
      machines: [{ id: "machine-a", name: "Machine A", workspace: "/a", capabilities: ["shell"] }],
    }));
    const routeB = fixture.socket();
    await fixture.broker.message(routeB.webSocket, JSON.stringify({
      type: "catalog",
      attachment_id: "machine-b",
      tools: [entry("beta")],
      machines: [{ id: "machine-b", name: "Machine B", workspace: "/b", capabilities: ["shell"] }],
    }));
    fixture.persistence.routes.get("user:machine-a")!.lease_expires_at = NOW;

    expect(fixture.broker.machines().map((machine) => machine.id)).toEqual(["machine-b"]);
    expect(routeA.closed).toMatchObject({ code: 1008 });
    expect(routeB.closed).toBeUndefined();
    expect(fixture.broker.provider().definitions().map((definition) => definition.name))
      .toEqual(["user_machine-b_beta"]);
  });

  it("keeps a dispatched call pinned when another named route is replaced", async () => {
    const fixture = createFixture();
    const routeA = fixture.socket();
    await fixture.broker.message(routeA.webSocket, JSON.stringify({
      type: "catalog",
      attachment_id: "route-a",
      tools: [entry("alpha")],
    }));
    const routeB = fixture.socket();
    await fixture.broker.message(routeB.webSocket, JSON.stringify({
      type: "catalog",
      attachment_id: "route-b",
      tools: [entry("beta")],
    }));
    const pending = fixture.broker.provider().resolve("alpha")!.handler({}, {
      sessionId: "session:1",
      callId: "source:1",
    });
    const call = routeA.sent.find((frame) => frame.type === "call")!;

    const replacementB = fixture.socket();
    await fixture.broker.message(replacementB.webSocket, JSON.stringify({
      type: "catalog",
      attachment_id: "route-b",
      tools: [entry("gamma")],
    }));
    expect(routeB.closed).toMatchObject({ code: 1008 });
    expect(routeA.closed).toBeUndefined();
    await fixture.broker.message(routeA.webSocket, result(call.call_id as string, "from A"));
    await expect(pending).resolves.toMatchObject({ success: true, output: "from A" });
    expect(replacementB.sent.some((frame) => frame.type === "call")).toBe(false);
  });

  it("still rejects duplicate unqualified generic tool names", async () => {
    const fixture = createFixture();
    const first = fixture.socket();
    await fixture.broker.message(first.webSocket, JSON.stringify({
      type: "catalog", attachment_id: "generic-a", tools: [entry("alpha")],
    }));
    const candidate = fixture.socket();
    await fixture.broker.message(candidate.webSocket, JSON.stringify({
      type: "catalog", attachment_id: "generic-b", tools: [entry("alpha")],
    }));
    expect(candidate.closed).toMatchObject({
      code: 1008,
      reason: expect.stringContaining("catalog_contract_mismatch"),
    });
    expect(first.closed).toBeUndefined();
    expect(fixture.broker.provider().definitions().map((definition) => definition.name)).toEqual(["alpha"]);
  });

  it("uses a stable bounded alias for a long qualified machine tool", async () => {
    const fixture = createFixture();
    const machineId = `m.${"a".repeat(120)}`;
    const originalName = `run_${"x".repeat(124)}`;
    const host = fixture.socket();
    const frame = {
      type: "catalog", attachment_id: machineId, tools: [entry(originalName)],
      machines: [{ id: machineId, name: "Long machine", workspace: "/long", capabilities: ["shell"] }],
    };
    await fixture.broker.message(host.webSocket, JSON.stringify(frame));
    const definition = fixture.broker.provider().definitions()[0]!;
    expect(definition.name).toHaveLength(128);
    expect(definition.name).toMatch(/^[A-Za-z0-9_-]{128}$/);
    expect(definition.description).toContain(`user:${machineId}:${originalName}`);
    const pending = fixture.broker.provider().resolve(definition.name)!.handler({}, {
      sessionId: "session:1", callId: "source:long",
    });
    const call = host.sent.find((candidate) => candidate.type === "call")!;
    expect(call.name).toBe(originalName);
    await fixture.broker.message(host.webSocket, result(call.call_id as string, "long route"));
    await expect(pending).resolves.toMatchObject({
      output: "long route",
      metadata: {
        machine_id: machineId,
        machine_name: "Long machine",
        tool_name: originalName,
      },
    });
  });

  it("removes machines when an open host lease expires", async () => {
    const fixture = createFixture();
    const host = fixture.socket();
    await fixture.broker.message(host.webSocket, JSON.stringify({
      type: "catalog",
      attachment_id: "laptop",
      tools: [entry()],
      machines: [{
        id: "laptop",
        name: "Laptop",
        workspace: "/workspace",
        capabilities: ["filesystem"],
      }],
    }));
    fixture.persistence.routes.get("user:laptop")!.lease_expires_at = NOW;

    expect(fixture.broker.machines()).toEqual([]);
    expect(host.closed).toMatchObject({ code: 1008 });
  });

  it("rejects machine metadata from Connect-grant hosts", async () => {
    const fixture = createFixture();
    const host = fixture.socket([], CLEANUP_DIGEST, GRANT_A);
    await fixture.broker.message(host.webSocket, JSON.stringify({
      type: "catalog",
      attachment_id: "desktop",
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

  it("keeps Connect and account persistence routes in separate authorization planes", async () => {
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
      expect(competing.sent).toEqual([{ type: "ready" }]);
      expect(first.closed).toBeUndefined();
      expect(competing.closed).toBeUndefined();
      expect([...fixture.persistence.routes.keys()]).toEqual(expect.arrayContaining([
        "connect:$legacy",
        "user:$legacy",
      ]));
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

  it("resumes every exact live route after a hibernating owner wakes", async () => {
    const fixture = createFixture();
    const left = fixture.socket();
    const right = fixture.socket();
    await fixture.broker.message(left.webSocket, JSON.stringify({
      type: "catalog",
      attachment_id: "machine-a",
      tools: [machineEntry("exec_command")],
      machines: [{ id: "machine-a", name: "Machine A", workspace: "/a", capabilities: ["shell"] }],
    }));
    await fixture.broker.message(right.webSocket, JSON.stringify({
      type: "catalog",
      attachment_id: "machine-b",
      tools: [machineEntry("exec_command")],
      machines: [{ id: "machine-b", name: "Machine B", workspace: "/b", capabilities: ["shell"] }],
    }));

    const resumed = new HostedToolsBroker(fixture.context, {
      persistence: fixture.persistence,
      now: () => NOW,
      resumeRetainedSockets: true,
    });

    expect(resumed.machines().map(({ id }) => id)).toEqual(["machine-a", "machine-b"]);
    expect(left.closed).toBeUndefined();
    expect(right.closed).toBeUndefined();
    expect(resumed.machineTool("machine-a", "exec_command")).toBeDefined();
    expect(resumed.machineTool("machine-b", "exec_command")).toBeDefined();
  });
});

function createFixture(
  entryAllowed?: (
    entry: HostedToolCatalogEntry,
    connectGrantId?: string,
    appToolCatalogDigest?: string,
    context?: HostedToolsAuthorizationContext,
  ) => boolean,
  options?: Readonly<{
    now?: () => number;
    renewLeasedAttachment?: (renewal: {
      expectedAttachmentId: string;
      fixedRouteId: string;
      renewalToken: string;
    }) => Promise<number | undefined>;
  }>,
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
    now: options?.now ?? (() => NOW),
    ...(options?.renewLeasedAttachment === undefined ? {} : {
      renewLeasedAttachment: options.renewLeasedAttachment,
    }),
    ...(entryAllowed === undefined ? {} : { entryAllowed }),
    randomUUID: () => ids.shift() ?? crypto.randomUUID(),
  });
  return {
    broker,
    context,
    persistence,
    socket(
      allowedMcpIds?: readonly string[],
      appToolCatalogDigest?: `0x${string}`,
      connectGrantId?: string,
      expectedAttachmentId?: string,
      maximumLeaseExpiresAt?: number,
      fixedRouteId?: string,
      renewalToken?: string,
    ) {
      const socket = new FakeSocket();
      socket.serializeAttachment({
        kind: "hosted-tools",
        sessionId: "session:route",
        ...(allowedMcpIds === undefined ? {} : { allowedMcpIds }),
        ...(appToolCatalogDigest === undefined ? {} : { appToolCatalogDigest }),
        ...(connectGrantId === undefined ? {} : { connectGrantId }),
        ...(expectedAttachmentId === undefined ? {} : { expectedAttachmentId }),
        ...(maximumLeaseExpiresAt === undefined ? {} : { maximumLeaseExpiresAt }),
        ...(fixedRouteId === undefined ? {} : { fixedRouteId }),
        ...(renewalToken === undefined ? {} : { renewalToken }),
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
  readonly routes = new Map<string, State>();

  constructor() {
    this.routes.set("user:$legacy", {
      route_id: "user:$legacy",
      generation: 0,
      host_id: null,
      lease_id: null,
      lease_expires_at: 0,
      catalog_json: null,
    });
  }

  get current(): State {
    return this.routes.get("user:$legacy")!;
  }

  initialize(_now: number): readonly State[] {
    return [...this.routes.values()]
      .filter((state) => state.lease_id !== null)
      .map((state) => structuredClone(state));
  }
  transaction<T>(callback: () => T): T { return callback(); }
  states(): readonly State[] { return [...this.routes.values()].map((row) => structuredClone(row)); }
  state(routeId: string): State | undefined {
    const row = this.routes.get(routeId);
    return row && structuredClone(row);
  }
  replaceHost(row: State): void { this.routes.set(row.route_id, structuredClone(row)); }
  clearHost(leaseId: string, generation: number): void {
    const current = [...this.routes.values()].find((row) => row.lease_id === leaseId
      && row.generation === generation);
    if (!current) return;
    this.routes.set(current.route_id, {
      ...current,
      host_id: null,
      lease_id: null,
      lease_expires_at: 0,
      catalog_json: null,
    });
  }
  clearCatalog(leaseId: string, generation: number): void {
    const current = [...this.routes.values()].find((row) => row.lease_id === leaseId
      && row.generation === generation);
    if (!current) return;
    this.routes.set(current.route_id, {
      ...current,
      catalog_json: null,
    });
  }
  readonly calls = new Map<string, CallRow>();
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
  pruneReceipts(_limit: number): void {}
}

async function catalog(broker: HostedToolsBroker, host: FakeSocket): Promise<void> {
  await broker.message(host.webSocket, JSON.stringify({ type: "catalog", tools: [entry()] }));
}

function entry(name = "fixture__lookup") {
  return {
    provider: "fixture",
    remote_name: name === "fixture__lookup" ? "lookup" : name,
    definition: {
      type: "function" as const,
      name,
      description: "Look up one fixture",
      strict: true,
      parameters: { type: "object", properties: {} },
    },
    parallel_safe: true,
    summary: "Fixture lookup",
    timeout_ms: 30_000,
  };
}

function machineEntry(name: "exec_command" | "write_stdin" | "preview") {
  const definitions = {
    exec_command: {
      parameters: EXEC_COMMAND_PARAMETERS,
      output_schema: EXECUTION_OUTPUT_SCHEMA,
    },
    write_stdin: {
      parameters: WRITE_STDIN_PARAMETERS,
      output_schema: EXECUTION_OUTPUT_SCHEMA,
    },
    preview: {
      parameters: MACHINE_PREVIEW_PARAMETERS,
      output_schema: PREVIEW_OUTPUT_SCHEMA,
    },
  };
  return {
    provider: "machine",
    remote_name: name,
    definition: {
      type: "function" as const,
      name,
      description: `Canonical machine ${name}`,
      strict: false,
      ...structuredClone(definitions[name]),
    },
    parallel_safe: name !== "write_stdin",
    summary: `Machine ${name}`,
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
