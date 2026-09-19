# CUA API reference and compatibility

The CUA reference is the provider installed with ChatGPT, inspected on September
18, 2026: `@oai/cua` 0.2.4, `@oai/cua-repl` 0.1.0, `@oai/sky` 0.6.32, and
`@oai/browser-desktop` 0.1.1. The provider is separate from `codex-rs`.

`runtime/src/cua_provider_provenance.json` in the experimental computer package
records source hashes. Its `cua_provider_tools.json` records the actual
`node_repl` MCP `tools/list` response with the installed CUA launcher's instruction
overrides. Capturing this catalog did not execute JavaScript, open an app, or
capture the screen. The lifecycle tool's hidden UI metadata is retained.

The public observation API binds an app or browser tab first:

```javascript
let app = await cua.getApp("Example App");
// In the next call, after reading the returned documentation and initial state:
await app.getScreenshot();
```

The same screenshot method is available on a bound tab. `{emit: false}` returns
image bytes without displaying them; the ordinary call emits an image as well as
returning the bytes. `getAXStateAndScreenshot()` returns and displays accessibility
state followed by the screenshot when one is available. The provider has no
public top-level `cua.getScreenshot()` or `cua.listWindows()` method. `getApp`
takes one target string, and native `drag` takes two points.

The updated provider also exposes `rewriteDocumentation()` and reports partial
inventory failures through `State.errors`. Its tool description instructs the
model to replay documentation when continuing a computer-use task after a
summary. The JavaScript input schema specifies a 30-second default timeout and
a title with a maximum length of 80 characters. Schema constraints and actual
argument parsing are checked separately; advertised constraints do not justify
inventing validation that the provider does not perform.

QuickJS remains the execution engine. API and contract conformance do not imply
that QuickJS implements every Node behavior, that every OS backend is identical,
or that the embedded browser implements every upstream DOM behavior. Provider
updates are pinned and reviewed; a package version alone is insufficient because
the installed CUA 0.2.4 source differs from the earlier 0.2.4 snapshot.

To verify an installed reference without running any provider or UI operation:

```sh
python3 scripts/codex-parity/cua-provider.py \
  --reference-root /path/to/node_modules/@oai
node --test js/nanocodex-computer/test/provider-contract.test.mjs
```
