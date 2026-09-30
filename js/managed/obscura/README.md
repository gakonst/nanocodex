# Obscura Wasm browser provider

The Nanocodex overlay adds `MANAGED_BROWSER_PROVIDER=obscura`. It uses the existing
Agents browser SDK against a local CDP implementation, with QuickJS Wasm,
Obscura DOM Wasm and the original Obscura JavaScript bootstrap loaded through
Worker Loader. The Obscura branch does not require `env.BROWSER`.

The existing hosted Chromium defaults are unchanged. The overlay is experimental and
has not been deployed. It includes a native `OBSCURA_NETWORK` service entrypoint
and default/development self-service bindings. This entrypoint uses the existing
anonymous public egress boundary and does not grant connector or Vault access.

## Scope

Implemented CDP commands are advertised through protocol discovery. Targets,
frames, default execution contexts, JavaScript evaluation, function calls,
remote object references, pierced DOM queries, focus and text insertion use the
actual Wasm DOM and QuickJS contexts. Unsupported commands return errors.
Host op arguments cross QuickJS as JSON so embedded NUL delimiters and attribute
values survive the FFI boundary. Text insertion appends to the focused editable
field through its native setter and emits cancellable `beforeinput` followed by
`input`; full keyboard and selection editing are not implemented.

Responses are bounded at 16 MiB. QuickJS's FFI output views are refreshed after
Wasm memory growth through a pinned package patch; large dynamically loaded
scripts can execute and their runtimes can be disposed normally. The patch
implements the memory-view repair proposed in upstream
[QuickJS PR #248](https://github.com/justjake/quickjs-emscripten/pull/248), which
addresses [issue #269](https://github.com/justjake/quickjs-emscripten/issues/269).
Both CommonJS and ESM builds are patched. Remove the patch when upgrading to an
upstream release containing the repair, after running the large-script journey.

Iframe navigation preserves URL fragments, including redirect inheritance.
`contentWindow` identity remains stable while a frame loads, and its load event
follows its parser scripts. Linked stylesheets retain fetched CSS and CSSOM edits
for rule inspection and load events; this does not provide visual rendering.
Stylesheet storage is bounded to 512 entries and 16 MiB per realm.

Screenshots, layout and coordinate input are not implemented. This is not a
complete Chromium/Puppeteer replacement. Named isolated worlds and full
cross-realm same-origin object identity are not implemented. Documents are
parsed before their scripts execute; streaming parser/event ordering differs.

Static ESM graphs support cycles, re-exports, live bindings and top-level await.
Only already-prefetched dynamic imports are supported. Import maps and redirected
module URLs are unsupported. Child frame messaging uses the sender's real host
frame identity, asynchronous dispatch and target-origin checks. The existing
bootstrap's structured-clone/transferable limitations still apply.

Browser state lives for one WebSocket execution. Cookies, local storage and tab
storage have a host-owned session implementation; snapshot serialization is
available separately but is not wired to account Durable Object storage.
IndexedDB remains in-memory per realm. The managed public gateway removes
response cookies and rejects cookie/authorization request headers, so it cannot
support authenticated website sessions. Cross-origin fetch and module loading enforce server origin permission,
credential-mode checks, preflight authorization and response-header exposure.
Cross-origin `no-cors` page fetches and fetch redirects that change origin remain
unsupported and fail closed. Classic scripts and frame HTML may load without
cookies. Cross-origin credential transport and authenticated sessions are not
implemented.

## Rebuild

Install repository dependencies, then rebuild the deployment assets with:

```
pnpm --filter nanocodex-managed-service run prepare:obscura
```

The checked-in generated Worker text and Wasm binaries are the assets consumed by
the provider. Editable runtime source and pinned dependency/provenance data are
included under `js/managed/obscura`. The DOM port is pinned to upstream
h4ckf0r0day/obscura revision 5ba6c05ed8dd848862a8a5a3d90e26fcae41ccd9 and
wasm-bindgen 0.2.126. It compiles the genuine DOM crate, not Obscura's native V8
runtime. `build-dom.sh` rebuilds those assets; QuickJS packages are 0.32.0.

Run `pnpm --filter nanocodex-managed-service test:obscura` for the packaged
browser journey. It exercises the deployed assets through the upstream Agents
browser connection helpers; full managed application and live-site validation
remain separate from that transport proof. See [the journey documentation](../test/obscura/README.md)
for reproducible evidence and the exact scope.
