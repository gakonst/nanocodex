import { submitFunctionCallOutput } from "../internal.mjs";

/**
 * Private host adapter, deliberately absent from the public Agent and Actions namespaces.
 * Bind this only after the trusted host receives an original pending call from the driver.
 * Rust remains responsible for validating the call ID and deduplicating operationId.
 */
export function functionCallOutputCapability(agent, callId) {
  if (typeof callId !== "string" || !callId.trim()) {
    throw new TypeError("callId must be a non-empty string");
  }
  return Object.freeze({
    submit: (options) => submitFunctionCallOutput(agent, callId, options),
  });
}
