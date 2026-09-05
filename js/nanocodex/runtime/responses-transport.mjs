const responsesTransports = new WeakMap();

export function createResponsesTransport(setup) {
  const transport = Object.freeze({});
  responsesTransports.set(transport, Object.freeze({ ...setup }));
  return transport;
}

export function resolveResponsesTransport(transport) {
  const setup = responsesTransports.get(transport);
  if (setup === undefined) {
    throw new TypeError("Agent.create requires a Responses transport");
  }
  return setup;
}

export function nonEmpty(value, label) {
  if (typeof value !== "string" || !value.trim()) {
    throw new TypeError(`${label} must be a non-empty string`);
  }
  return value;
}
