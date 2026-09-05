export type ConnectorCompletion = Readonly<{
  type: "nanocodex:connector-complete";
  connector: string;
  result: "success" | "error";
  error?: string | undefined;
  message?: string | undefined;
}>;

export function isConnectorCompletion(value: unknown): value is ConnectorCompletion {
  return isRecord(value)
    && Object.keys(value).every((key) => completionKeys.has(key))
    && value.type === "nanocodex:connector-complete"
    && typeof value.connector === "string"
    && value.connector.length > 0
    && (value.result === "success" || value.result === "error")
    && (value.error === undefined || typeof value.error === "string")
    && (value.message === undefined || typeof value.message === "string");
}

const completionKeys = new Set(["type", "connector", "result", "error", "message"]);

export function connectorCompletionFor(
  event: Readonly<{ data: unknown; origin: string; source: unknown }>,
  expected: Readonly<{ connector: string; origin: string; source: unknown }>,
): ConnectorCompletion | undefined {
  return event.origin === expected.origin
    && event.source === expected.source
    && isConnectorCompletion(event.data)
    && event.data.connector === expected.connector
    ? event.data
    : undefined;
}

export function connectorCompletion(
  connector: string,
  result: "connected" | "cancelled" | "failed",
): ConnectorCompletion {
  if (result === "connected") {
    return { type: "nanocodex:connector-complete", connector, result: "success" };
  }
  return {
    type: "nanocodex:connector-complete",
    connector,
    result: "error",
    error: result === "cancelled" ? "connector_authorization_cancelled" : "connector_authorization_failed",
    message: result === "cancelled"
      ? "The account authorization was cancelled. Connect again when you are ready."
      : "The account provider could not complete authorization. Try connecting again.",
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
