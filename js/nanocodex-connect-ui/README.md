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

The Connect UI separates sign-in from authorization. Both steps fill the page without an overlay, rounded shell, or shadow. The sign-in form stays compact;
authorization pairs the requesting app and its origin with a review of accounts
and permissions. Desktop uses a two-column review; mobile stacks the same
information above persistent actions. The requester mark is an initial derived
from the validated app name, and Nanocodex uses its canonical mark.

The standalone host follows system appearance unless `data-theme="light"` or
`data-theme="dark"` is set. Styles remain scoped to `.connect-onboarding`.
The page owns its opaque background; the host frame fills the viewport. Reduced-motion preferences disable
control transitions. See the Connect dialog browser journeys for reproducible
light/dark mobile and desktop checks.

### Appearance

Pass visual tokens to the reusable component:

```tsx
<ConnectOnboarding
  host={host}
  request={request}
  appearance={{
    theme: "system",
    accentColor: "#635bff",
    fontFamily: '"Avenir Next", -apple-system, sans-serif',
    borderRadius: 8,
  }}
/>
```

`theme` accepts `light`, `dark`, or `system`. Omit it to inherit the host's theme.
`accentColor` accepts six-digit hex; button text automatically uses black or white
for contrast. `borderRadius` accepts 0–24 pixels. `fontFamily` selects installed
fonts, or fonts loaded by your self-hosted page; hosted Connect does not fetch
fonts from your application. Invalid values fall back to defaults. Tokens are
scoped to the component and reset when removed, so multiple instances do not
change each other's theme. Appearance does not change app identity or grants.

The default typography uses system fonts, restrained medium-weight headings, and
compact control labels. SMS sign-in uses a centered single column at every width. Authorization uses two columns with a 36px gap on desktop; mobile stacks. Both use the same 24px medium-weight headings and font family.

### Standalone service consent

Signed `urn:nanocodex:services:` resources display the exact Vault item IDs,
website origins, phone number IDs, and requested actions. Phone read permission
explicitly includes incoming verification messages; provisioning and release
require later account approval. A services-only request does not display agent
execution permission. Invalid or duplicate service resources reject the request
before authorization. Enforcement remains in the Connect API and service broker.
