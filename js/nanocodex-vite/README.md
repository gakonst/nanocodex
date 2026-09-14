# `nanocodex-vite`

Vite integration for the `nanocodex` JavaScript package. The plugin builds the
current Rust/WASM bindings when it runs from a source checkout, installs the
browser compatibility aliases in page and nested Worker graphs, and exposes a
same-origin ChatGPT subscription socket during local development.

The WASM build normalizes wasm-bindgen byte views for all three JavaScript
targets. Cloudflare's V8 `subarray` offset check can reject valid strings above
the 128 MiB address boundary after WASM memory grows. Direct bounded views keep
those transfers valid without changing the WASM ABI. The build validates the
generated helper shapes, and changes to the normalization script invalidate
the binding cache.

```js
import { nanocodex } from "nanocodex-vite";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [nanocodex()],
  worker: { format: "es" },
});
```

Cloudflare applications use the combined entry instead of installing a second
Cloudflare plugin:

```js
import { nanocodex } from "nanocodex-vite/cloudflare";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react(), nanocodex()],
});
```

In development, the application Worker remains the sole browser credential
broker. Nanocodex gives workerd only the current access-token snapshot and a
capability-scoped loopback egress; it never imports the host process
environment. `vite build` does not read local auth or include those bindings.
The refresh token, ID token, full auth document, and credentials never enter
browser code or responses.

Managed local applications can pass `oauthRelay: true`; the plugin then owns
the fixed provider callback relay for the lifetime of the Vite server. The
relay signing and callback helpers are exported from
`nanocodex-vite/oauth-relay`.
