import {
  localConnectorAuthorization,
  localMcpAuthorization,
  wrapLocalConnectorAuthorizationState,
  wrapLocalMcpAuthorizationState,
  type LocalConnectorAuthorization,
  type LocalConnectorFlow,
  type LocalMcpAuthorization,
} from "../oauth-relay.mjs";

const flow: LocalConnectorFlow = "connect";
const connector: LocalConnectorAuthorization | undefined = localConnectorAuthorization(
  "https://nanocodex.localhost",
  "github",
  flow,
);
if (connector) {
  const wrapped: URL = await wrapLocalConnectorAuthorizationState(
    new URL("https://github.com/login/oauth/authorize?state=provider-state"),
    connector,
    "development-secret-with-at-least-32-bytes",
  );
  void wrapped;
}

const mcp: LocalMcpAuthorization | undefined = localMcpAuthorization(
  "https://nanocodex.localhost",
  "a".repeat(43),
  "managed",
);
if (mcp) {
  await wrapLocalMcpAuthorizationState(
    new URL("https://mcp.example/authorize?state=provider-state"),
    mcp,
    "development-secret-with-at-least-32-bytes",
  );
}

// @ts-expect-error Unsupported flow is rejected by the public contract.
localConnectorAuthorization("https://nanocodex.localhost", "github", "other");
