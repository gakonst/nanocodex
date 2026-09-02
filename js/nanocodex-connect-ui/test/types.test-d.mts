import {
  appVisibilityPermissions,
  focusedConnectorFromResources,
  mcpConnectionsFromWire,
  parseConnectPolicy,
  type ConnectPolicy,
  type McpConnection,
  type McpConnectionStatus,
} from "nanocodex-connect-ui/connectPolicy.mjs";
import type { ConnectorCapability } from "nanocodex-connect-ui/connectorPolicy.mjs";

const wireValue: unknown = [];
const policy: ConnectPolicy = parseConnectPolicy(wireValue);
const connections: readonly McpConnection[] = mcpConnectionsFromWire(wireValue);
const focus: ConnectorCapability | undefined =
  focusedConnectorFromResources([], []);
const labels: readonly string[] = appVisibilityPermissions([]).map(({ label }) => label);

// @ts-expect-error MCP connection states are a closed protocol union.
const invalidStatus: McpConnectionStatus = "pending";

void policy;
void connections;
void focus;
void labels;
void invalidStatus;
