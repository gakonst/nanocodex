export const googleConnectorCapabilities = Object.freeze([
  "gmail",
  "gdrive",
  "gcalendar",
  "gtasks",
  "gdocs",
  "gsheets",
  "gslides",
  "gcontacts",
]);

export const connectorCapabilityIds = Object.freeze([
  "github",
  ...googleConnectorCapabilities,
  "slack",
  "x",
  "chatgpt",
]);

export const connectorProviderIds = Object.freeze(["github", "google", "slack", "x", "chatgpt"]);
export const connectorConnectionHeader = "X-Nanocodex-Connector-Connection";

const capabilityIds = new Set(connectorCapabilityIds);
const providerIds = new Set(connectorProviderIds);
const connectionId = /^[A-Za-z0-9_-]{43}$/;
const maxConnections = 64;
const maxLabelLength = 256;

export function connectorProviderFor(capability) {
  if (!capabilityIds.has(capability)) return undefined;
  return googleConnectorCapabilities.includes(capability) ? "google" : capability;
}

export function connectorCapabilityLabel(capability) {
  if (capability === "github") return "GitHub";
  if (capability === "gmail") return "Gmail";
  if (capability === "gdrive") return "Drive";
  if (capability === "gcalendar") return "Calendar";
  if (capability === "gtasks") return "Tasks";
  if (capability === "gdocs") return "Docs";
  if (capability === "gsheets") return "Sheets";
  if (capability === "gslides") return "Slides";
  if (capability === "gcontacts") return "Contacts";
  if (capability === "slack") return "Slack";
  if (capability === "x") return "X";
  if (capability === "chatgpt") return "ChatGPT";
  return undefined;
}

export function connectorStatusesFromWire(value) {
  if (!isRecord(value)) throw new Error("Nanocodex received invalid connector statuses.");
  const statuses = {};
  for (const [capability, candidate] of Object.entries(value)) {
    if (!capabilityIds.has(capability)) {
      throw new Error("Nanocodex received invalid connector statuses.");
    }
    statuses[capability] = decodeStatus(candidate, capability);
  }
  return Object.freeze(statuses);
}

export function connectorConnectionsForCapabilities(statuses, capabilities) {
  if (!isRecord(statuses) || !Array.isArray(capabilities)) {
    throw new Error("Nanocodex received invalid connector statuses.");
  }
  const requested = [...new Set(capabilities)];
  if (requested.some((capability) => !capabilityIds.has(capability))) {
    throw new Error("Nanocodex received invalid connector statuses.");
  }
  const merged = new Map();
  for (const capability of requested) {
    const status = statuses[capability];
    if (!isRecord(status) || !Array.isArray(status.connections)) continue;
    for (const candidate of status.connections) {
      const connection = decodeConnection(candidate, capability);
      const current = merged.get(connection.id);
      if (current && (current.label !== connection.label
        || (current.account_id && connection.account_id && current.account_id !== connection.account_id))) {
        throw new Error("Nanocodex received inconsistent connector identities.");
      }
      const granted = new Set([
        ...(current?.capabilities ?? []),
        ...(connection.capabilities ?? [capability]),
        capability,
      ]);
      merged.set(connection.id, Object.freeze({
        id: connection.id,
        label: connection.label,
        ...(current?.account_id || connection.account_id
          ? { account_id: current?.account_id ?? connection.account_id }
          : {}),
        capabilities: Object.freeze(connectorCapabilityIds.filter((id) => granted.has(id))),
      }));
    }
  }
  return Object.freeze([...merged.values()]);
}

export function connectorProviderMatchesCapabilities(provider, capabilities) {
  if (!providerIds.has(provider) || !Array.isArray(capabilities)) return false;
  return capabilities.some((capability) => connectorProviderFor(capability) === provider);
}

export function connectorAttemptedCapabilitiesConnected(capabilities, statuses) {
  if (!Array.isArray(capabilities)
    || capabilities.length === 0
    || capabilities.some((capability) => !capabilityIds.has(capability))) {
    throw new Error("Nanocodex received invalid connector capabilities.");
  }
  const decoded = connectorStatusesFromWire(statuses);
  return capabilities.some((capability) => decoded[capability]?.connected === true);
}

export function connectorControlsForCapabilities(capabilities, statuses) {
  if (!Array.isArray(capabilities)
    || capabilities.some((capability) => !capabilityIds.has(capability))) {
    throw new Error("Nanocodex received invalid connector capabilities.");
  }
  const resolvedStatuses = statuses === undefined ? undefined : connectorStatusesFromWire(statuses);
  const grouped = new Map();
  for (const capability of capabilities) {
    const provider = connectorProviderFor(capability);
    const current = grouped.get(provider) ?? [];
    if (!current.includes(capability)) current.push(capability);
    grouped.set(provider, current);
  }
  return Object.freeze([...grouped.entries()].map(([provider, requested]) => {
    const connectedCapabilities = resolvedStatuses
      ? requested.filter((capability) => resolvedStatuses[capability]?.connected === true)
      : [];
    const missingCapabilities = requested.filter((capability) => !connectedCapabilities.includes(capability));
    return Object.freeze({
      provider,
      capabilities: Object.freeze([...requested]),
      connectedCapabilities: Object.freeze(connectedCapabilities),
      missingCapabilities: Object.freeze(missingCapabilities),
      connections: resolvedStatuses
        ? connectorConnectionsForCapabilities(resolvedStatuses, requested)
        : Object.freeze([]),
      connected: resolvedStatuses !== undefined && missingCapabilities.length === 0,
      partial: connectedCapabilities.length > 0 && missingCapabilities.length > 0,
      resolved: resolvedStatuses !== undefined,
    });
  }));
}

function decodeStatus(value, capability) {
  if (!isRecord(value)
    || Object.keys(value).some((key) => !["connected", "connections", "account_id", "connection_id", "label"].includes(key))
    || typeof value.connected !== "boolean"
    || (value.account_id !== undefined && !shortText(value.account_id))
    || (value.connection_id !== undefined && (typeof value.connection_id !== "string" || !connectionId.test(value.connection_id)))
    || (value.label !== undefined && !displayLabel(value.label))
    || (value.connections !== undefined && (!Array.isArray(value.connections)
      || value.connections.length > maxConnections))) {
    throw new Error("Nanocodex received invalid connector statuses.");
  }
  const connections = value.connections === undefined
    ? []
    : value.connections.map((connection) => decodeConnection(connection, capability));
  if (new Set(connections.map(({ id }) => id)).size !== connections.length) {
    throw new Error("Nanocodex received invalid connector statuses.");
  }
  return Object.freeze({
    connected: value.connected,
    connections: Object.freeze(connections),
    ...(value.account_id === undefined ? {} : { account_id: value.account_id.trim() }),
    ...(value.connection_id === undefined ? {} : { connection_id: value.connection_id }),
    ...(value.label === undefined ? {} : { label: value.label.trim() }),
  });
}

function decodeConnection(value, capability) {
  if (!isRecord(value)
    || Object.keys(value).some((key) => !["id", "label", "account_id", "capabilities"].includes(key))
    || typeof value.id !== "string" || !connectionId.test(value.id)
    || !displayLabel(value.label)
    || (value.account_id !== undefined && !shortText(value.account_id))
    || (value.capabilities !== undefined && (!Array.isArray(value.capabilities)
      || value.capabilities.length > connectorCapabilityIds.length
      || value.capabilities.some((item) => typeof item !== "string" || !capabilityIds.has(item))
      || value.capabilities.some((item) => connectorProviderFor(item) !== connectorProviderFor(capability))
      || new Set(value.capabilities).size !== value.capabilities.length))) {
    throw new Error("Nanocodex received invalid connector statuses.");
  }
  const capabilities = value.capabilities ?? [capability];
  return Object.freeze({
    id: value.id,
    label: value.label.trim(),
    ...(value.account_id === undefined ? {} : { account_id: value.account_id.trim() }),
    capabilities: Object.freeze([...capabilities]),
  });
}

function displayLabel(value) {
  return typeof value === "string" && value.trim().length > 0 && value.trim().length <= maxLabelLength;
}

function shortText(value) {
  return typeof value === "string" && value.trim().length > 0 && value.trim().length <= maxLabelLength;
}

function isRecord(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
