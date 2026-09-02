# nanocodex-connect-protocol

Low-level, runtime-independent framing for Connect connector callback state.

```js
import {
  isScopedConnectConnectorState,
  scopedConnectConnectorState,
  unscopedConnectConnectorState,
} from "nanocodex-connect-protocol";

const callbackState = scopedConnectConnectorState("0123456789abcdef");
isScopedConnectConnectorState(callbackState); // true
unscopedConnectConnectorState(callbackState); // "0123456789abcdef"
```

The package also owns the secret-free completion frame used to correlate OAuth
popup callbacks across `postMessage`, same-origin storage events, and
`BroadcastChannel`. The frame contains only a connector identifier, an
unguessable browser correlation state, and a terminal result.
