# Browser CDN PoC

This page runs the Rust/WASM agent directly from the published npm package. It
has no package manifest, dependency installation, bundler, or framework. Any
static host, including a PHP application, can serve `index.html` unchanged.

The page expects an application-authorized Responses WebSocket at
`/api/responses`. Browser WebSockets cannot attach OpenAI's authorization
header, so the API key must stay in that endpoint. The Cloudflare Worker under
`../react-vite/worker` is the repository's complete example of that thin relay.

To use another authorized endpoint, add it to the page URL:

```text
https://app.example/demo?endpoint=wss://app.example/api/responses
```

The package version is intentionally pinned in the CDN import. Update and test
that pin explicitly when adopting a new Nanocodex release.
