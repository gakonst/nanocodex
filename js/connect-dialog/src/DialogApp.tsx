import { useCallback, useSyncExternalStore } from "react";

import { ConnectOnboarding } from "nanocodex-connect-ui/App";
import { parentDialog } from "./protocol";
import { appearanceFromSearch } from "./appearance";
import { OAuthConsent } from "./OAuthConsent";

export function App() {
  return new URLSearchParams(window.location.search).has("oauth_request")
    ? <OAuthConsent /> : <ParentDialog />;
}

function ParentDialog() {
  const subscribe = useCallback(
    (listener: () => void) => parentDialog.subscribe?.(listener) ?? (() => {}),
    [],
  );
  const getSnapshot = useCallback(() => parentDialog.getRequest?.(), []);
  const request = useSyncExternalStore(subscribe, getSnapshot, () => undefined);
  const appearance = appearanceFromSearch(window.location.search);
  return <ConnectOnboarding appearance={appearance} host={parentDialog} request={request} />;
}
