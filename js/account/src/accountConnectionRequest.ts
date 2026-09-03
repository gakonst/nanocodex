import type { ToolActivity } from "nanocodex-react/agent";
import { connectorCompletionFor } from "nanocodex-connect-ui/connectorCompletion";
import {
  connectorCapabilityIds,
  connectorCapabilityLabel,
  connectorProviderFor,
  connectorStatusesFromWire,
  type ConnectorCapability,
  type ConnectorProvider,
} from "nanocodex-connect-ui/connectorPolicy.mjs";
import { isRecord, responseFailure } from "./accountSessionRequest.ts";

export type RequestedAccountConnection = Readonly<{
  connector: Exclude<ConnectorCapability, "chatgpt">;
  label: string;
}>;

const requestToolName = "requestAccountConnection";
const supportedConnectors = new Set<ConnectorCapability>(
  connectorCapabilityIds.filter((connector) => connector !== "chatgpt"),
);
let accountConnectionInFlight = false;

export function requestedAccountConnection(
  tool: ToolActivity,
): RequestedAccountConnection | undefined {
  if (tool.name !== requestToolName || tool.status !== "completed") return undefined;
  const output = parseJson(tool.output ?? tool.result);
  if (!isRecord(output)
    || Object.keys(output).some((key) => !["status", "action", "connector", "label"].includes(key))
    || output.status !== "user_action_required"
    || output.action !== "connect_account"
    || typeof output.connector !== "string"
    || typeof output.label !== "string"
    || !supportedConnectors.has(output.connector as ConnectorCapability)) return undefined;
  const connector = output.connector as RequestedAccountConnection["connector"];
  const label = connectorCapabilityLabel(connector);
  return label ? { connector, label } : undefined;
}

export async function connectRequestedAccount(
  request: RequestedAccountConnection,
  signal: AbortSignal,
): Promise<void> {
  signal.throwIfAborted();
  if (accountConnectionInFlight) {
    throw new Error("Another account connection is already in progress.");
  }
  accountConnectionInFlight = true;
  let popup: Window | null = null;

  try {
    const provider = connectorProviderFor(request.connector) as Exclude<ConnectorProvider, "chatgpt">;
    popup = window.open(
      "about:blank",
      "nanocodex-account-connector",
      "popup,width=520,height=720",
    );
    if (!popup) {
      throw new Error("The account authorization popup was blocked. Allow popups and try again.");
    }
    const response = await connectorRequest(`/v1/connectors/${provider}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ return_to: "/connect" }),
      signal,
    });
    if (!response.ok) {
      throw await responseFailure(response, `Couldn’t connect ${request.label}.`);
    }
    const body: unknown = await response.json();
    if (!isRecord(body) || typeof body.authorization_url !== "string") {
      throw new Error("Invalid connector authorization response.");
    }
    const authorizationUrl = new URL(body.authorization_url);
    if (authorizationUrl.protocol !== "https:") {
      throw new Error("Invalid connector authorization URL.");
    }
    if (popup.closed) {
      throw new Error("The account authorization popup was closed before it started.");
    }

    const completion = waitForConnectorCompletion(popup, provider, signal);
    popup.location.href = authorizationUrl.href;
    const result = await completion;
    if (result.result !== "success") {
      throw new Error(result.message ?? `Couldn’t connect ${request.label}.`);
    }
    await verifyAccountConnection(request, signal);
  } finally {
    if (popup && !popup.closed) popup.close();
    accountConnectionInFlight = false;
  }
}

async function verifyAccountConnection(
  request: RequestedAccountConnection,
  signal: AbortSignal,
): Promise<void> {
  const response = await connectorRequest("/v1/connectors", { signal });
  if (!response.ok) {
    throw await responseFailure(response, `Couldn’t verify the ${request.label} connection.`);
  }
  const body: unknown = await response.json();
  if (!isRecord(body) || !isRecord(body.connectors)) {
    throw new Error("Invalid connector response.");
  }
  const statuses = connectorStatusesFromWire(body.connectors);
  if (statuses[request.connector]?.connected !== true) {
    throw new Error(`The provider completed without connecting ${request.label}.`);
  }
}

function waitForConnectorCompletion(
  popup: Window,
  provider: Exclude<ConnectorProvider, "chatgpt">,
  signal: AbortSignal,
): Promise<NonNullable<ReturnType<typeof connectorCompletionFor>>> {
  return new Promise((resolve, reject) => {
    let popupClosed: number | undefined;
    const dispose = () => {
      window.removeEventListener("message", onMessage);
      window.clearInterval(popupCheck);
      if (popupClosed !== undefined) window.clearTimeout(popupClosed);
      signal.removeEventListener("abort", onAbort);
    };
    const fail = (error: Error) => {
      dispose();
      reject(error);
    };
    const onMessage = (event: MessageEvent<unknown>) => {
      const completion = connectorCompletionFor(event, {
        connector: provider,
        origin: window.location.origin,
        source: popup,
      });
      if (!completion) return;
      dispose();
      resolve(completion);
    };
    const onAbort = () => fail(new DOMException("The account connection was cancelled.", "AbortError"));
    const popupCheck = window.setInterval(() => {
      if (!popup.closed || popupClosed !== undefined) return;
      popupClosed = window.setTimeout(() => {
        fail(new Error("The account authorization popup was closed before it completed. Connect again when you are ready."));
      }, 750);
    }, 300);
    window.addEventListener("message", onMessage);
    signal.addEventListener("abort", onAbort, { once: true });
    if (signal.aborted) onAbort();
  });
}

async function connectorRequest(path: string, init: RequestInit = {}): Promise<Response> {
  return fetch(path, {
    ...init,
    cache: "no-store",
    credentials: "same-origin",
    headers: {
      accept: "application/json",
      ...Object.fromEntries(new Headers(init.headers)),
    },
  });
}

function parseJson(value: string | undefined): unknown {
  if (!value) return undefined;
  try { return JSON.parse(value); } catch { return undefined; }
}
