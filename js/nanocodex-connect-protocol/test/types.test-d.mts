import {
  isScopedConnectConnectorState,
  scopedConnectConnectorState,
  unscopedConnectConnectorState,
} from "nanocodex-connect-protocol";

const unknownState: unknown = "0123456789abcdef";
const scoped: string = scopedConnectConnectorState(unknownState);
const unscoped: string | undefined = unscopedConnectConnectorState(scoped);

if (isScopedConnectConnectorState(unknownState)) {
  const narrowed: string = unknownState;
  void narrowed;
}

// @ts-expect-error Invalid framed states can produce undefined.
const definitelyUnscoped: string = unscopedConnectConnectorState(unknownState);

void unscoped;
void definitelyUnscoped;
