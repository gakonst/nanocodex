import { describe, expect, it } from "vitest";

import {
  DEVICE_HOST_PROTOCOL_VERSION,
  MAX_DEVICE_HOST_MESSAGE_BYTES,
  deviceToolAmbiguous,
  deviceToolResult,
  deviceToolUnavailable,
  matchesDeviceHostLease,
  parseDeviceHostCommand,
  parseDeviceToolInput,
} from "../src/device-host-protocol";

const HOST_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const LEASE_ID = "22222222-2222-4222-8222-222222222222";

describe("Android device-host protocol", () => {
  it("strictly parses v1 attach, ping, and device results", () => {
    expect(parseDeviceHostCommand(JSON.stringify({
      type: "attach",
      protocol_version: DEVICE_HOST_PROTOCOL_VERSION,
      host_id: HOST_ID,
      catalog_version: 1,
    }))).toEqual({
      type: "attach",
      protocol_version: 1,
      host_id: HOST_ID,
      catalog_version: 1,
    });
    expect(parseDeviceHostCommand(JSON.stringify({
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
    expect(parseDeviceHostCommand(JSON.stringify({
      type: "device_tool_result",
      lease_id: LEASE_ID,
      epoch: 7,
      call_id: "phone-call:1",
      success: false,
      output: { code: "permission_denied", remediation: "grant calendar access" },
    }))).toMatchObject({ type: "device_tool_result", success: false });

    expect(() => parseDeviceHostCommand(JSON.stringify({
      type: "attach",
      protocol_version: 1,
      host_id: HOST_ID,
      catalog_version: 1,
      credential: "must-not-be-accepted",
    }))).toThrow("unsupported fields");
    expect(() => parseDeviceHostCommand(JSON.stringify({
      type: "attach",
      protocol_version: 2,
      host_id: HOST_ID,
      catalog_version: 1,
    }))).toThrow("unsupported device-host protocol version");
  });

  it("enforces message, identity, catalog, operation, and object bounds", () => {
    expect(() => parseDeviceHostCommand("x".repeat(MAX_DEVICE_HOST_MESSAGE_BYTES + 1)))
      .toThrow("limited");
    expect(() => parseDeviceHostCommand(JSON.stringify({
      type: "attach",
      protocol_version: 1,
      host_id: HOST_ID.toUpperCase(),
      catalog_version: 1,
    }))).toThrow("lowercase UUID v4");
    expect(() => parseDeviceHostCommand(JSON.stringify({
      type: "attach",
      protocol_version: 1,
      host_id: HOST_ID,
      catalog_version: 0,
    }))).toThrow("catalog_version");
    expect(parseDeviceToolInput({ operation: "calendar.list" })).toEqual({
      operation: "calendar.list",
      arguments: {},
    });
    expect(() => parseDeviceToolInput({ operation: "calendar.list", arguments: [] }))
      .toThrow("phone arguments must be an object");
    expect(() => parseDeviceToolInput({ operation: "calendar/list" }))
      .toThrow("operation must be");
  });

  it("fences stale identities, epochs, and expired leases", () => {
    const state = {
      host_id: HOST_ID,
      lease_id: LEASE_ID,
      epoch: 9,
      lease_expires_at: 20_000,
    };
    expect(matchesDeviceHostLease({ hostId: HOST_ID, leaseId: LEASE_ID, epoch: 9 }, state, 19_999))
      .toBe(true);
    expect(matchesDeviceHostLease({ hostId: HOST_ID, leaseId: LEASE_ID, epoch: 8 }, state, 19_999))
      .toBe(false);
    expect(matchesDeviceHostLease({ hostId: HOST_ID, leaseId: LEASE_ID, epoch: 9 }, state, 20_001))
      .toBe(false);
    expect(matchesDeviceHostLease({ hostId: HOST_ID, leaseId: crypto.randomUUID(), epoch: 9 }, state, 19_999))
      .toBe(false);
  });

  it("keeps failed, offline, and ambiguous device outcomes structured", () => {
    expect(deviceToolResult(false, { code: "not_allowed" })).toEqual({
      ok: false,
      status: "failed",
      output: { code: "not_allowed" },
    });
    expect(deviceToolUnavailable()).toEqual({
      ok: false,
      status: "unavailable",
      message: "No Android device host is currently attached.",
    });
    expect(deviceToolAmbiguous("disconnected after dispatch")).toEqual({
      ok: false,
      status: "ambiguous",
      message: "disconnected after dispatch",
    });
  });
});
