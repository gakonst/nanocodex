# Experimental computer runtime

An independent implementation imported from `sky-re-1000502/rebuild`, exposing
the recovered CUA JavaScript interface through JSON-RPC/MCP. Nanocodex owns the
launcher and attachment adapters; this workspace owns OS and browser operations.

The import contains authored Rust/JavaScript implementation, synthetic fixtures,
and behavioral oracle data. It uses the normal published `rquickjs` crate and does not load the
original Sky executable, service bundle, signing identity or private service
socket. Interface documentation and app guidance are adapted from the research
contract. This is experimental compatibility work, not a claim of complete Sky
service parity.

The runtime uses native macOS APIs, Linux X11/XTEST, and explicitly configured
Chromium CDP. Windows UIA/capture provider code is retained for development.
Optional providers, IAB routing and V8 require explicit host configuration. The
Nanocodex adapters forward trusted provider/runtime configuration. The Chrome
extension is embedded in the companion and exported with `extension-export`;
installation and bridge registration remain explicit.

`serve` handles newline-delimited MCP on stdin/stdout. `eval`, `call`,
`capabilities` and `permissions` are useful for development. `--fixture` uses
synthetic controls. The adapter passes `--allow-native-control` because the
embedding application already authorized its native tool capability; configured
app/origin restrictions and OS grants remain enforced. Standalone execution
without that flag retains the runtime's explicit app approval flow.

The nested workspace keeps the companion build independent from the agent's
JavaScript engine. Production builds do not depend on vendored QuickJS sources,
the research checkout, or original binaries.

Build and test from the repository root with `pnpm build:computer` and
`pnpm test:computer`. See the [adapter README](../README.md) for installation,
configuration and the execution-mode matrix.
The [compatibility record](../PARITY.md) separates tested public behavior from
remaining native, browser and host integration gaps.
