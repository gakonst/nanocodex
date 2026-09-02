const connectStatePrefix = "connect.";
const brokerState = /^[A-Za-z0-9_-]{16,480}$/;
const callbackCompletionState = /^[A-Za-z0-9_-]{43}$/;
const callbackCompletionKeys = new Set(["type", "connector", "state", "result", "error", "message"]);
const callbackCompletionStoragePrefix = "nanocodex:oauth-completion:";
const callbackCompletionChannelPrefix = "nanocodex-oauth-completion-";

type UnknownRecord = Record<string, unknown>;

export type CallbackCompletion = Readonly<{
  type: "nanocodex:callback-complete";
  connector: string;
  state: string;
  result: "success" | "error";
  error?: string | undefined;
  message?: string | undefined;
}>;

type CallbackCompletionParameters = Readonly<{
  connector: string;
  state: string;
  result: "success" | "error";
  error?: string | undefined;
  message?: string | undefined;
}>;

/** Frames a valid broker authorization state for a Connect callback. */
export function scopedConnectConnectorState(value: unknown): string {
  if (typeof value !== "string" || !brokerState.test(value)) {
    throw new Error("The connector authorization state is invalid.");
  }
  return `${connectStatePrefix}${value}`;
}

/** Reports whether a value is a framed Connect connector callback state. */
export function isScopedConnectConnectorState(value: unknown): value is string {
  return typeof value === "string"
    && value.startsWith(connectStatePrefix)
    && brokerState.test(value.slice(connectStatePrefix.length));
}

/** Returns the broker state from a valid framed callback state. */
export function unscopedConnectConnectorState(value: unknown): string | undefined {
  return isScopedConnectConnectorState(value)
    ? value.slice(connectStatePrefix.length)
    : undefined;
}

export function callbackCompletion(parameters: CallbackCompletionParameters): CallbackCompletion {
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

export function isCallbackCompletion(value: unknown): value is CallbackCompletion {
  return isRecord(value)
    && Object.keys(value).every((key) => callbackCompletionKeys.has(key))
    && value.type === "nanocodex:callback-complete"
    && validCallbackConnector(value.connector)
    && validCallbackCompletionState(value.state)
    && (value.result === "success" || value.result === "error")
    && (value.error === undefined || validCallbackText(value.error))
    && (value.message === undefined || validCallbackText(value.message));
}

export function callbackCompletionFor(
  value: unknown,
  expected: Readonly<{ connector: string; state: string }>,
): CallbackCompletion | undefined {
  return isCallbackCompletion(value)
    && value.connector === expected?.connector
    && value.state === expected?.state
    ? value
    : undefined;
}

export function callbackCompletionStorageKey(state: string): string {
  requireCallbackCompletionState(state);
  return `${callbackCompletionStoragePrefix}${state}`;
}

export function callbackCompletionChannelName(state: string): string {
  requireCallbackCompletionState(state);
  return `${callbackCompletionChannelPrefix}${state}`;
}

export function isCallbackCompletionState(value: unknown): value is string {
  return validCallbackCompletionState(value);
}

function requireCallbackCompletionState(state: unknown): asserts state is string {
  if (!validCallbackCompletionState(state)) {
    throw new Error("The callback completion state is invalid.");
  }
}

function validCallbackCompletionState(value: unknown): value is string {
  return typeof value === "string" && callbackCompletionState.test(value);
}

function validCallbackConnector(value: unknown): value is string {
  return typeof value === "string"
    && value.length >= 1
    && value.length <= 256
    && !/[\u0000-\u001f\u007f]/u.test(value);
}

function validCallbackText(value: unknown): value is string {
  return typeof value === "string"
    && value.length >= 1
    && value.length <= 1_024
    && !/[\u0000\u007f]/u.test(value);
}

function isRecord(value: unknown): value is UnknownRecord {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
