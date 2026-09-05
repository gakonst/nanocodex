# nanocodex-connect-ui

Reusable React account and Connect onboarding surfaces for Nanocodex.

```tsx
import {
  ConnectOnboarding,
  type ConnectOnboardingHost,
} from "nanocodex-connect-ui/App";
import "nanocodex-connect-ui/styles.css";

export function Approval({ host, request }) {
  return <ConnectOnboarding host={host} request={request} />;
}
```

The package owns browser presentation and ceremony orchestration. It does not
issue grants or enforce server-side Connect authority.
