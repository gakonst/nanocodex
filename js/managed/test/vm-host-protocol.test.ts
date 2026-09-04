import { describe, expect, it } from "vitest";

import {
  MAX_VM_HOST_MESSAGE_BYTES,
  VM_HOST_PROTOCOL_VERSION,
  matchesVmHostLease,
  parseVmHostCommand,
  type VmHostServerMessage,
} from "../src/vm-host-protocol";

const HOST_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const LEASE_ID = "22222222-2222-4222-8222-222222222222";
const ALLOCATION_ID = "33333333-3333-4333-8333-333333333333";

describe("VM-host control protocol", () => {
  it("strictly parses the v1 attach and heartbeat commands", () => {
    expect(parseVmHostCommand(JSON.stringify({
      type: "attach",
      protocol_version: VM_HOST_PROTOCOL_VERSION,
      host_id: HOST_ID,
      factory_name: "garage-mac",
      max_vms: 4,
      vm: { cpus: 2, memory_mib: 1_024 },
    }))).toEqual({
      type: "attach",
      protocol_version: 1,
      host_id: HOST_ID,
      factory_name: "garage-mac",
      max_vms: 4,
      vm: { cpus: 2, memory_mib: 1_024 },
    });
    expect(parseVmHostCommand(JSON.stringify({
      type: "ping",
      lease_id: LEASE_ID,
      epoch: 7,
      nonce: "heartbeat-7",
    }))).toEqual({
      type: "ping",
      lease_id: LEASE_ID,
      epoch: 7,
      nonce: "heartbeat-7",
    });
    expect(parseVmHostCommand(JSON.stringify({
      type: "ping",
      lease_id: LEASE_ID,
      epoch: 7,
    }))).toEqual({ type: "ping", lease_id: LEASE_ID, epoch: 7 });
  });

  it("strictly parses provision, release, and reconciliation acknowledgements", () => {
    const identity = {
      lease_id: LEASE_ID,
      epoch: 7,
      allocation_id: ALLOCATION_ID,
      generation: 3,
      machine_id: "vm:mount-1",
    };
    expect(parseVmHostCommand(JSON.stringify({ type: "provisioned", ...identity })))
      .toEqual({ type: "provisioned", ...identity });
    expect(parseVmHostCommand(JSON.stringify({ type: "released", ...identity })))
      .toEqual({ type: "released", ...identity });
    expect(parseVmHostCommand(JSON.stringify({
      type: "reconcile",
      lease_id: LEASE_ID,
      epoch: 7,
      allocations: [{
        allocation_id: ALLOCATION_ID,
        generation: 3,
        machine_id: "vm:mount-1",
        state: "ready",
      }],
    }))).toEqual({
      type: "reconcile",
      lease_id: LEASE_ID,
      epoch: 7,
      allocations: [{
        allocation_id: ALLOCATION_ID,
        generation: 3,
        machine_id: "vm:mount-1",
        state: "ready",
      }],
    });
  });

  it("rejects unsupported versions, directions, extra fields, and malformed JSON", () => {
    expect(() => parseVmHostCommand(JSON.stringify({
      type: "attach",
      protocol_version: 2,
      host_id: HOST_ID,
      factory_name: "garage-mac",
      max_vms: 4,
      vm: { cpus: 2, memory_mib: 1_024 },
    }))).toThrow("unsupported VM-host protocol version");
    expect(() => parseVmHostCommand(JSON.stringify({
      type: "attach",
      protocol_version: 1,
      host_id: HOST_ID,
      factory_name: "garage-mac",
      max_vms: 4,
      vm: { cpus: 2, memory_mib: 1_024 },
      account_id: "must-not-be-accepted",
    }))).toThrow("unsupported field account_id");
    expect(() => parseVmHostCommand(JSON.stringify({
      type: "attach",
      protocol_version: 1,
      host_id: HOST_ID,
      max_vms: 4,
      vm: { cpus: 2, memory_mib: 1_024 },
    }))).toThrow("factory_name");
    expect(() => parseVmHostCommand(JSON.stringify({
      type: "attach",
      protocol_version: 1,
      host_id: HOST_ID,
      factory_name: "garage-mac",
      max_vms: 4,
      vm: { cpus: 2, memory_mib: 1_024, network: true },
    }))).toThrow("unsupported field network");
    expect(() => parseVmHostCommand(JSON.stringify({ type: "lease" })))
      .toThrow("unsupported VM-host command");
    expect(() => parseVmHostCommand("{"))
      .toThrow("VM-host messages must be JSON objects");
  });

  it("enforces byte, identity, integer, nonce, and VM shape bounds", () => {
    expect(() => parseVmHostCommand("x".repeat(MAX_VM_HOST_MESSAGE_BYTES + 1)))
      .toThrow("limited");
    for (const [field, value] of [
      ["host_id", HOST_ID.toUpperCase()],
      ["factory_name", "Garage Mac"],
      ["factory_name", "cf_sandbox"],
      ["factory_name", "cloudflare"],
      ["factory_name", "host"],
      ["max_vms", 0],
      ["max_vms", 65],
    ] as const) {
      const attach = {
        type: "attach",
        protocol_version: 1,
        host_id: HOST_ID,
        factory_name: "garage-mac",
        max_vms: 4,
        vm: { cpus: 2, memory_mib: 1_024 },
        [field]: value,
      };
      expect(() => parseVmHostCommand(JSON.stringify(attach))).toThrow();
    }
    for (const vm of [
      { cpus: 0, memory_mib: 1_024 },
      { cpus: 65, memory_mib: 1_024 },
      { cpus: 2, memory_mib: 127 },
      { cpus: 2, memory_mib: 262_145 },
    ]) {
      expect(() => parseVmHostCommand(JSON.stringify({
        type: "attach",
        protocol_version: 1,
        host_id: HOST_ID,
        factory_name: "garage-mac",
        max_vms: 4,
        vm,
      }))).toThrow();
    }
    expect(() => parseVmHostCommand(JSON.stringify({
      type: "ping",
      lease_id: LEASE_ID,
      epoch: 0,
    }))).toThrow("epoch");
    expect(() => parseVmHostCommand(JSON.stringify({
      type: "ping",
      lease_id: LEASE_ID,
      epoch: 1,
      nonce: "x".repeat(129),
    }))).toThrow("nonce");
  });

  it("bounds and de-duplicates exact reconciliation entries", () => {
    const allocation = {
      allocation_id: ALLOCATION_ID,
      generation: 1,
      machine_id: "vm:mount-1",
      state: "ready",
    };
    const reconcile = (allocations: unknown[]) => JSON.stringify({
      type: "reconcile",
      lease_id: LEASE_ID,
      epoch: 1,
      allocations,
    });
    expect(() => parseVmHostCommand(reconcile(Array.from({ length: 65 }, () => allocation))))
      .toThrow("at most 64");
    expect(() => parseVmHostCommand(reconcile([allocation, allocation])))
      .toThrow("unique allocation_id and machine_id");
    expect(() => parseVmHostCommand(reconcile([{ ...allocation, state: "releasing" }])))
      .toThrow("state must be ready");
    expect(() => parseVmHostCommand(reconcile([{ ...allocation, owner_id: "secret" }])))
      .toThrow("unsupported field owner_id");
  });

  it("fences independent host leases and allocation generations", () => {
    const lease = {
      host_id: HOST_ID,
      lease_id: LEASE_ID,
      epoch: 9,
      lease_expires_at: 20_000,
    };
    expect(matchesVmHostLease({ hostId: HOST_ID, leaseId: LEASE_ID, epoch: 9 }, lease, 19_999))
      .toBe(true);
    expect(matchesVmHostLease({
      hostId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
      leaseId: LEASE_ID,
      epoch: 9,
    }, lease, 19_999)).toBe(false);
    expect(matchesVmHostLease({ hostId: HOST_ID, leaseId: LEASE_ID, epoch: 8 }, lease, 19_999))
      .toBe(false);
    expect(matchesVmHostLease({ hostId: HOST_ID, leaseId: LEASE_ID, epoch: 9 }, lease, 20_001))
      .toBe(false);

  });

  it("keeps the server frame union pinned to lease and allocation generations", () => {
    const messages: VmHostServerMessage[] = [
      {
        type: "lease",
        protocol_version: 1,
        lease_id: LEASE_ID,
        epoch: 2,
        expires_at: 20_000,
        max_vms: 4,
        vm: { cpus: 2, memory_mib: 1_024 },
      },
      { type: "pong", lease_id: LEASE_ID, epoch: 2, expires_at: 20_000, nonce: "n" },
      {
        type: "provision",
        lease_id: LEASE_ID,
        epoch: 2,
        allocation_id: ALLOCATION_ID,
        generation: 3,
        slot: 0,
        machine_id: "vm:mount-1",
        tool_attachment: {
          url: "wss://managed.example/v1/agents/agent-1/tool-host",
          bearer: "a".repeat(43),
        },
      },
      {
        type: "release",
        lease_id: LEASE_ID,
        epoch: 2,
        allocation_id: ALLOCATION_ID,
        generation: 3,
        machine_id: "vm:mount-1",
      },
      { type: "fenced", epoch: 3, reason: "host lease replaced" },
      { type: "error", code: "capacity_exhausted", message: "no VM slots remain" },
    ];
    expect(messages.map(({ type }) => type)).toEqual([
      "lease",
      "pong",
      "provision",
      "release",
      "fenced",
      "error",
    ]);
  });
});
