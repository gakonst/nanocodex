import {
  connectorConnectionHeader,
  connectorControlsForCapabilities,
  connectorConnectionsForCapabilities,
  connectorProviderFor,
  connectorStatusesFromWire,
  googleConnectorCapabilities,
  type ConnectorCapability,
  type ConnectorControl,
  type ConnectorConnection,
  type ConnectorProvider,
  type ConnectorStatuses,
} from "nanocodex-connect-ui/connectorPolicy.mjs";

const provider: ConnectorProvider | undefined = connectorProviderFor("gdocs");
const capabilities: readonly ConnectorCapability[] = googleConnectorCapabilities;
const statuses: ConnectorStatuses = connectorStatusesFromWire({});
const connections: readonly ConnectorConnection[] = connectorConnectionsForCapabilities(
  statuses,
  capabilities,
);
const controls: readonly ConnectorControl[] = connectorControlsForCapabilities(capabilities, statuses);
const header: "X-Nanocodex-Connector-Connection" = connectorConnectionHeader;

void provider;
void connections;
void controls;
void header;

// @ts-expect-error `google` controls OAuth but is never a grant capability.
const invalidCapability: ConnectorCapability = "google";
void invalidCapability;
