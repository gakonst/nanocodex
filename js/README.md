# JavaScript libraries

- [`nanocodex`](nanocodex) publishes `nanocodex`: runtime-specific `Agent`
  namespaces, domain-grouped `Actions`, decorators, and Node/browser WASM hosts.
- [`nanocodex-tools`](nanocodex-tools) owns reusable JavaScript-only tool
  infrastructure: routing and Code Mode, attachments and hosted-tool protocol,
  memory parsing/ranking, workspace/shell/Git/SSH adapters, and standard tools.
  It is a leaf package and never imports `nanocodex` or generated WASM.
- [`nanocodex-react`](nanocodex-react) provides `nanocodex-react`: the external store, provider,
  and hooks for a browser Worker owned by the embedding application.
- [`nanocodex-vite`](nanocodex-vite) owns the Nanocodex Vite plugin, WASM build, local OAuth relay,
  sibling development-application mounts, and Cloudflare Vite integration.
- [`nanocodex-connect-protocol`](nanocodex-connect-protocol) owns the low-level,
  UI-independent Connect callback framing shared by the public edge and Connect Worker.
- [`nanocodex-connect-ui`](nanocodex-connect-ui) owns reusable account selection,
  connector management, and Connect onboarding React surfaces shared by the account
  product and standalone Connect dialog.
- [`nanocodex-terminal`](nanocodex-terminal) provides `nanocodex-terminal`: controlled React
  transcript and composer components with an optional canonical stylesheet.
- [`account`](account), [`connect-dialog`](connect-dialog), and
  [`connect-playground`](connect-playground) are product applications.
- [`managed`](managed), [`egress`](egress), and [`connect-api`](connect-api) are
  independently deployable Cloudflare Workers. `mcp-target.mts` owns their small
  shared remote-target security boundary.

The registry packages include the low-level `nanocodex-tools`, the headless
`nanocodex` binding, and `nanocodex-vite`.
`nanocodex-react` owns semantic conversation state through its headless Agent
controller. `nanocodex-terminal` renders that state without creating Agents,
choosing transports, or owning credentials and persistence. Generated
`wasm-bindgen` output stays private to `nanocodex` and is produced by the Vite
package.
