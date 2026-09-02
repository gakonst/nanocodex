const connectStatePrefix = "connect.";
const brokerState = /^[A-Za-z0-9_-]{16,480}$/;
const callbackCompletionState = /^[A-Za-z0-9_-]{43}$/;
const callbackCompletionKeys = new Set(["type", "connector", "state", "result", "error", "message"]);
const callbackCompletionStoragePrefix = "nanocodex:oauth-completion:";
const callbackCompletionChannelPrefix = "nanocodex-oauth-completion-";

export function scopedConnectConnectorState(value) {
  if (typeof value !== "string" || !brokerState.test(value)) {
    throw new Error("The connector authorization state is invalid.");
  }
  return `${connectStatePrefix}${value}`;
}

export function isScopedConnectConnectorState(value) {
  return typeof value === "string"
    && value.startsWith(connectStatePrefix)
    && brokerState.test(value.slice(connectStatePrefix.length));
}

export function unscopedConnectConnectorState(value) {
  return isScopedConnectConnectorState(value)
    ? value.slice(connectStatePrefix.length)
    : undefined;
}

export function callbackCompletion(parameters) {
  const completion = {
    type: "nanocodex:callback-complete",
    connector: parameters?.connector,
    state: parameters?.state,
    result: parameters?.result,
    ...(parameters?.error === undefined ? {} : { error: parameters.error }),
    ...(parameters?.message === undefined ? {} : { message: parameters.message }),
  };
  if (!isCallbackCompletion(completion)) {
    throw new Error("The callback completion is invalid.");
  }
  return completion;
}

export function isCallbackCompletion(value) {
  return value !== null
    && typeof value === "object"
    && !Array.isArray(value)
    && Object.keys(value).every((key) => callbackCompletionKeys.has(key))
    && value.type === "nanocodex:callback-complete"
    && validCallbackConnector(value.connector)
    && validCallbackCompletionState(value.state)
    && (value.result === "success" || value.result === "error")
    && (value.error === undefined || validCallbackText(value.error))
    && (value.message === undefined || validCallbackText(value.message));
}

export function callbackCompletionFor(value, expected) {
  return isCallbackCompletion(value)
    && value.connector === expected?.connector
    && value.state === expected?.state
    ? value
    : undefined;
}

export function callbackCompletionStorageKey(state) {
  requireCallbackCompletionState(state);
  return `${callbackCompletionStoragePrefix}${state}`;
}

export function callbackCompletionChannelName(state) {
  requireCallbackCompletionState(state);
  return `${callbackCompletionChannelPrefix}${state}`;
}

export function isCallbackCompletionState(value) {
  return validCallbackCompletionState(value);
}

function requireCallbackCompletionState(state) {
  if (!validCallbackCompletionState(state)) {
    throw new Error("The callback completion state is invalid.");
  }
}

function validCallbackCompletionState(value) {
  return typeof value === "string" && callbackCompletionState.test(value);
}

function validCallbackConnector(value) {
  return typeof value === "string"
    && value.length >= 1
    && value.length <= 256
    && !/[\u0000-\u001f\u007f]/u.test(value);
}

function validCallbackText(value) {
  return typeof value === "string"
    && value.length >= 1
    && value.length <= 1_024
    && !/[\u0000\u007f]/u.test(value);
}
