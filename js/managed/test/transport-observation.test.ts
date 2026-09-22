import { expect, it } from "vitest";
import type { AgentEvent } from "nanocodex";
import { transportObservation } from "../src/transport-observation";

it("correlates retries without retaining the upstream error or raw frames", () => {
  const detail = transportObservation({ type: "model.attempt.retrying", request_id: "runtime-request",
    payload: { attempt: 1, next_attempt: 2, max_attempts: 3, error_class: "websocket_closed",
      failure_phase: "receive", delay_ns: 192_000_000, connection_generation: 1,
      opens_new_socket: true, server_requested_delay: false, error: "secret credential and prompt",
      api_event: { authorization: "secret" }, replay_mode: "full_history" },
  } as unknown as AgentEvent, "managed-turn");
  expect(detail).toMatchObject({ request_id: "runtime-request", turn_id: "managed-turn", outcome: "retrying",
    error_kind: "websocket_closed", retry_delay_ms: 192, failure_phase: "receive", opens_new_socket: true,
    attempt_count: 1, next_attempt: 2, connection_generation: 1 });
  expect(JSON.stringify(detail)).not.toContain("secret");
  expect(detail).not.toHaveProperty("error");
});
