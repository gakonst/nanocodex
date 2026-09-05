import { useCallback, useSyncExternalStore } from "react";

import { ConnectOnboarding } from "nanocodex-connect-ui/App";
import { parentDialog } from "./protocol";

export function App() {
  const subscribe = useCallback(
    (listener: () => void) => parentDialog.subscribe?.(listener) ?? (() => {}),
    [],
  );
  const getSnapshot = useCallback(() => parentDialog.getRequest?.(), []);
  const request = useSyncExternalStore(subscribe, getSnapshot, () => undefined);
  return <ConnectOnboarding host={parentDialog} request={request} />;
}
