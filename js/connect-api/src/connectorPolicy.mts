export const connectorCapabilities = Object.freeze([
  "github",
  "gmail",
  "gdrive",
  "gcalendar",
  "gtasks",
  "gdocs",
  "gsheets",
  "gslides",
  "gcontacts",
  "slack",
  "x",
  "chatgpt",
] as const);

export const oauthConnectorProviders = Object.freeze(["github", "google", "slack", "x"] as const);

export type ConnectorCapability = typeof connectorCapabilities[number];
export type OAuthConnectorProvider = typeof oauthConnectorProviders[number];
export type RoutableConnectorCapability = Exclude<ConnectorCapability, "chatgpt">;
export type ConnectorConnection = Readonly<{
  id: string;
  label: string;
  account_id?: string;
  capabilities?: readonly ConnectorCapability[];
}>;
export type ConnectorStatus = Readonly<{
  connected: boolean;
  connections: readonly ConnectorConnection[];
  label?: string;
  account_id?: string;
}>;
export type ConnectorConnectionSnapshot = Readonly<
  Partial<Record<ConnectorCapability, readonly string[]>>
>;

const connectorCapabilitySet = new Set(connectorCapabilities);
const connectionIdPattern = /^[A-Za-z0-9_-]{43}$/;

export class ConnectorPolicyFailure extends Error {
  readonly status: number;
  readonly code: string;

  constructor(status: number, code: string, message: string) {
    super(message);
    this.status = status;
    this.code = code;
  }
}

export function isConnectorCapability(value: unknown): value is ConnectorCapability {
  return typeof value === "string"
    && connectorCapabilitySet.has(value as ConnectorCapability);
}

export function isConnectorConnectionId(value: unknown): value is string {
  return typeof value === "string" && connectionIdPattern.test(value);
}

export function connectorProvider(value: unknown): OAuthConnectorProvider | undefined {
  if (value === "github" || value === "slack" || value === "x") return value;
  if (typeof value === "string" && [
    "gmail",
    "gdrive",
    "gcalendar",
    "gtasks",
    "gdocs",
    "gsheets",
    "gslides",
    "gcontacts",
  ].includes(value)) return "google";
  return undefined;
}

export function connectorCapabilityForUrl(url: URL): RoutableConnectorCapability | undefined {
  if (url.origin === "https://api.github.com") return "github";
  if (url.origin === "https://gmail.googleapis.com") return "gmail";
  if (url.origin === "https://www.googleapis.com"
    && /^(?:\/drive\/v3|\/upload\/drive\/v3)(?:\/|$)/.test(url.pathname)) return "gdrive";
  if (url.origin === "https://www.googleapis.com"
    && /^\/calendar\/v3(?:\/|$)/.test(url.pathname)) return "gcalendar";
  if (url.origin === "https://tasks.googleapis.com") return "gtasks";
  if (url.origin === "https://docs.googleapis.com") return "gdocs";
  if (url.origin === "https://sheets.googleapis.com") return "gsheets";
  if (url.origin === "https://slides.googleapis.com") return "gslides";
  if (url.origin === "https://people.googleapis.com") return "gcontacts";
  if (url.origin === "https://slack.com") return "slack";
  if (url.origin === "https://api.x.com") return "x";
  return undefined;
}

export function connectorRequestTarget(
  capability: RoutableConnectorCapability,
  value: unknown,
): URL {
  if (typeof value !== "string" || value.length === 0 || value.length > 8_192
    || !value.startsWith("/") || value.startsWith("//")) {
    throw new ConnectorPolicyFailure(400, "invalid_connector_path", "The connector request path is invalid.");
  }
  const origin = capability === "github" ? "https://api.github.com"
    : capability === "gmail" ? "https://gmail.googleapis.com"
      : capability === "gdrive" || capability === "gcalendar" ? "https://www.googleapis.com"
        : capability === "gtasks" ? "https://tasks.googleapis.com"
          : capability === "gdocs" ? "https://docs.googleapis.com"
            : capability === "gsheets" ? "https://sheets.googleapis.com"
              : capability === "gslides" ? "https://slides.googleapis.com"
                : capability === "gcontacts" ? "https://people.googleapis.com"
                  : capability === "slack" ? "https://slack.com"
                    : capability === "x" ? "https://api.x.com" : undefined;
  if (!origin) {
    throw new ConnectorPolicyFailure(403, "connector_destination_denied", "The connector destination is not allowed.");
  }
  const target = new URL(value, origin);
  const canonicalPath = capability === "github"
    || (!target.pathname.includes("\\") && !/%(?:2e|2f|5c|25)/i.test(target.pathname));
  const pathAllowed = capability === "github"
    || (capability === "gmail" && /^\/gmail\/v1\/users\/me(?:\/|$)/.test(target.pathname))
    || (capability === "gdrive" && /^(?:\/drive\/v3|\/upload\/drive\/v3)(?:\/|$)/.test(target.pathname))
    || (capability === "gcalendar" && /^\/calendar\/v3(?:\/|$)/.test(target.pathname))
    || (capability === "gtasks" && /^\/tasks\/v1(?:\/|$)/.test(target.pathname))
    || (capability === "gdocs" && /^\/v1\/documents(?:\/|$)/.test(target.pathname))
    || (capability === "gsheets" && /^\/v4\/spreadsheets(?:\/|$)/.test(target.pathname))
    || (capability === "gslides" && /^\/v1\/presentations(?:\/|$)/.test(target.pathname))
    || (capability === "gcontacts" && /^\/v1\/(?:people|contactGroups|otherContacts)(?:\/|:|$)/.test(target.pathname))
    || (capability === "slack" && /^\/api\/[A-Za-z0-9._-]+$/.test(target.pathname))
    || (capability === "x" && /^\/2\/(?:tweets|users|lists|dm_(?:conversations|events)|media)(?:\/|$)/.test(target.pathname));
  if (target.origin !== origin || target.username || target.password || target.hash
    || !canonicalPath || !pathAllowed) {
    throw new ConnectorPolicyFailure(403, "connector_destination_denied", "The connector destination is not allowed.");
  }
  let count = 0;
  for (const [name, queryValue] of target.searchParams) {
    count += 1;
    if (count > 64 || name.length > 128 || queryValue.length > 4_096
      || /^(?:access_token|api_key|authorization|key|oauth_token|token)$/i.test(name)) {
      throw new ConnectorPolicyFailure(403, "connector_destination_denied", "The connector query is not allowed.");
    }
  }
  return target;
}

export function publicConnectorStatus(value: unknown): ConnectorStatus {
  if (!record(value) || value.connected !== true) {
    return Object.freeze({ connected: false, connections: Object.freeze([]) });
  }

  const legacyLabel = optionalDisplayString(value.label);
  const legacyAccountId = optionalDisplayString(value.account_id);
  let connections: ConnectorConnection[] = [];
  if (value.connections !== undefined) {
    if (!Array.isArray(value.connections) || value.connections.length > 64) {
      invalidBrokerMetadata();
    }
    connections = value.connections.map(publicConnectorConnection);
    if (new Set(connections.map(({ id }) => id)).size !== connections.length) {
      invalidBrokerMetadata("The connector broker returned duplicate connection identities.");
    }
  } else if (value.connection_id !== undefined) {
    if (!isConnectorConnectionId(value.connection_id) || !legacyLabel) invalidBrokerMetadata();
    // Transitional readers accept the shape shipped by the first Google
    // multi-account rollout and normalize it into the provider-neutral list.
    connections = [Object.freeze({
      id: value.connection_id,
      label: legacyLabel,
      ...(legacyAccountId ? { account_id: legacyAccountId } : {}),
    })];
  }

  const frozenConnections = Object.freeze(connections);
  if (value.connections !== undefined && frozenConnections.length === 0) {
    return Object.freeze({ connected: false, connections: frozenConnections });
  }
  const sole = frozenConnections.length === 1 ? frozenConnections[0] : undefined;
  const label = legacyLabel ?? sole?.label;
  const accountId = legacyAccountId ?? sole?.account_id;
  return Object.freeze({
    connected: true,
    connections: frozenConnections,
    ...(label ? { label } : {}),
    ...(accountId ? { account_id: accountId } : {}),
  });
}

export function connectorConnectionSnapshot(
  statuses: Readonly<Record<ConnectorCapability, ConnectorStatus>>,
  capabilities: readonly ConnectorCapability[] = connectorCapabilities,
): ConnectorConnectionSnapshot {
  const snapshot: Partial<Record<ConnectorCapability, readonly string[]>> = {};
  for (const capability of capabilities) {
    const status = statuses[capability];
    if (!status?.connected || !Array.isArray(status.connections) || status.connections.length === 0) continue;
    snapshot[capability] = Object.freeze(status.connections.map(({ id }) => id));
  }
  return Object.freeze(snapshot);
}

export function isConnectorConnectionSnapshot(
  value: unknown,
): value is ConnectorConnectionSnapshot {
  if (!record(value)) return false;
  return Object.entries(value).every(([capability, ids]) => (
    isConnectorCapability(capability)
    && Array.isArray(ids)
    && ids.length <= 64
    && ids.every(isConnectorConnectionId)
    && new Set(ids).size === ids.length
  ));
}

export function intersectConnectorConnectionSnapshot(
  approved: ConnectorConnectionSnapshot | undefined,
  statuses: Readonly<Record<ConnectorCapability, ConnectorStatus>>,
  requested: readonly ConnectorCapability[],
): ConnectorConnectionSnapshot | undefined {
  if (approved === undefined) return undefined;
  const selected: Partial<Record<ConnectorCapability, readonly string[]>> = {};
  for (const capability of requested) {
    if (!Object.hasOwn(approved, capability)) continue;
    const approvedIds = approved[capability] ?? [];
    const liveIds = new Set((statuses[capability]?.connections ?? []).map(({ id }) => id));
    selected[capability] = Object.freeze(approvedIds.filter((id) => liveIds.has(id)));
  }
  return Object.freeze(selected);
}

export function completeConnectorConnectionSnapshot(
  approved: ConnectorConnectionSnapshot | undefined,
  connectedAtApproval: readonly ConnectorCapability[] | undefined,
  statuses: Readonly<Record<ConnectorCapability, ConnectorStatus>>,
  requested: readonly ConnectorCapability[],
): Readonly<{
  connectorConnections: ConnectorConnectionSnapshot | undefined;
  legacyConnectorCapabilities: readonly ConnectorCapability[];
}> {
  if (approved === undefined) {
    throw new ConnectorPolicyFailure(
      409,
      "connector_approval_snapshot_required",
      "This pending approval predates exact connector identities and must be retried.",
    );
  }
  const selected: Partial<Record<ConnectorCapability, readonly string[]>> = {};
  const legacy: ConnectorCapability[] = [];
  const previouslyConnected = connectedAtApproval === undefined
    ? undefined
    : new Set(connectedAtApproval);
  for (const capability of requested) {
    if (capability === "chatgpt") continue;
    const status = statuses[capability];
    const liveIds = (status?.connections ?? []).map(({ id }) => id);
    if (Object.hasOwn(approved, capability)) {
      const live = new Set(liveIds);
      selected[capability] = Object.freeze(
        (approved[capability] ?? []).filter((id) => live.has(id)),
      );
      continue;
    }

    // A missing entry is eligible for late binding only when the current
    // approval explicitly recorded that the connector was disconnected. This
    // is the fresh-account flow where OAuth happens inside the dialog after
    // signing. Missing rollout metadata or a connected-but-unidentified
    // account cannot be widened into whichever identity happens to be live.
    if (previouslyConnected === undefined) {
      if (status?.connected) {
        throw new ConnectorPolicyFailure(
          409,
          "connector_approval_snapshot_incomplete",
          "The approval did not bind this connected account to a stable identity.",
        );
      }
      continue;
    }
    if (status?.connected && liveIds.length === 0) {
      legacy.push(capability);
      continue;
    }
    if (previouslyConnected.has(capability)) {
      if (status?.connected) {
        throw new ConnectorPolicyFailure(
          409,
          "connector_approval_snapshot_incomplete",
          "The approval did not bind this connected account to a stable identity.",
        );
      }
      continue;
    }
    if (liveIds.length > 0) selected[capability] = Object.freeze(liveIds);
  }
  if (legacy.length > 0 && Object.keys(selected).length > 0) {
    throw new ConnectorPolicyFailure(
      409,
      "connector_identity_modes_mixed",
      "Exact and legacy connector identities cannot be combined in one new grant.",
    );
  }
  return Object.freeze({
    connectorConnections: legacy.length > 0 ? undefined : Object.freeze(selected),
    legacyConnectorCapabilities: Object.freeze(legacy),
  });
}

export function resolveConnectorConnection(
  snapshot: ConnectorConnectionSnapshot | undefined,
  capability: ConnectorCapability,
  bodySelector: unknown,
  headerSelector: unknown,
): string | undefined {
  const body = optionalSelector(bodySelector);
  const header = optionalSelector(headerSelector);
  if (body !== undefined && header !== undefined && body !== header) {
    throw new ConnectorPolicyFailure(
      400,
      "connector_connection_invalid",
      "Connector connection selectors disagree.",
    );
  }
  const selected = body ?? header;
  if (selected !== undefined && !isConnectorConnectionId(selected)) {
    throw new ConnectorPolicyFailure(
      400,
      "connector_connection_invalid",
      "The connector connection ID is invalid.",
    );
  }

  // Grants written before connection snapshots remain usable only through
  // their original selector-less capability semantics. A caller may not use
  // such a grant to opt into an identity connected after approval.
  if (snapshot === undefined) {
    if (selected !== undefined) {
      throw new ConnectorPolicyFailure(
        403,
        "connector_connection_not_granted",
        "This legacy grant does not authorize a connector connection selector.",
      );
    }
    return undefined;
  }

  const granted = snapshot[capability] ?? [];
  if (selected === undefined) {
    if (granted.length === 1) return granted[0];
    if (granted.length > 1) {
      throw new ConnectorPolicyFailure(
        409,
        "connector_connection_required",
        "Choose which connected account to use.",
      );
    }
    throw new ConnectorPolicyFailure(
      403,
      "connector_connection_not_granted",
      "This grant does not authorize a connected account for this capability.",
    );
  }
  if (!granted.includes(selected)) {
    throw new ConnectorPolicyFailure(
      403,
      "connector_connection_not_granted",
      "The connected account is outside this grant.",
    );
  }
  return selected;
}

export function applyConnectorConnectionSelector(
  headers: Headers,
  snapshot: ConnectorConnectionSnapshot | undefined,
  capability: ConnectorCapability,
  bodySelector: unknown,
): string | undefined {
  const connectionId = resolveConnectorConnection(
    snapshot,
    capability,
    bodySelector,
    headers.get("x-nanocodex-connector-connection"),
  );
  headers.delete("x-nanocodex-connector-connection");
  if (connectionId) headers.set("x-nanocodex-connector-connection", connectionId);
  return connectionId;
}

function publicConnectorConnection(value: unknown): ConnectorConnection {
  if (!record(value) || !isConnectorConnectionId(value.id)) invalidBrokerMetadata();
  const label = optionalDisplayString(value.label);
  const accountId = optionalDisplayString(value.account_id);
  if (!label) invalidBrokerMetadata();
  let capabilities;
  if (value.capabilities !== undefined) {
    if (!Array.isArray(value.capabilities)
      || value.capabilities.length > connectorCapabilities.length
      || !value.capabilities.every(isConnectorCapability)
      || new Set(value.capabilities).size !== value.capabilities.length) {
      invalidBrokerMetadata();
    }
    capabilities = Object.freeze([...value.capabilities]);
  }
  return Object.freeze({
    id: value.id,
    label,
    ...(accountId ? { account_id: accountId } : {}),
    ...(capabilities ? { capabilities } : {}),
  });
}

function optionalSelector(value: unknown): string | undefined {
  if (value === undefined || value === null || value === "") return undefined;
  if (typeof value !== "string") {
    throw new ConnectorPolicyFailure(
      400,
      "connector_connection_invalid",
      "The connector connection ID is invalid.",
    );
  }
  return value;
}

function optionalDisplayString(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim();
  return trimmed.length > 0 && trimmed.length <= 256 ? trimmed : undefined;
}

function invalidBrokerMetadata(
  message = "The connector broker returned invalid connection metadata.",
): never {
  throw new ConnectorPolicyFailure(502, "connector_broker_invalid", message);
}

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
