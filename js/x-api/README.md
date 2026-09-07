# Nanocodex X API

`nanocodex-x` is a private Cloudflare Worker for reading public X data. Managed
agents call the native `browseX` tool; `accountInfo().apis` advertises it even
when no X connector is authorized. The managed Worker reaches it through the
`NANOCODEX_X` Service Binding. There is no public route, workers.dev endpoint,
API key, or dependency on the x.md hosted service.

The shared tool and request contract live at `nanocodex-tools/x`. The Worker
owns provider access, conversion, rendering, caching, and the HTTP interface.

```js
await tools.browseX({ action: "post", url: "https://x.com/jack/status/20" });
await tools.browseX({ action: "profile", handle: "example" });
await tools.browseX({ action: "search", q: "from:example workers", feed: "latest" });
await tools.browseX({ action: "followers", handle: "example", limit: 20 });
await tools.browseX({ action: "following", handle: "example", cursor: "returned-nextCursor" });
```

The native tool returns structured data including `markdown`, canonical post
links, provider source, and cache status. Post conversion preserves quoted
posts, media/video details, parent/thread context, ranked replies, and article
content when upstream supplies them. `thread`, `context`, `replies`, `userinfo`,
and `full` control post output. Profiles filter replies and reposts from their
latest posts. Browse results are bounded to 20 items; cursor pagination is
preferred, and page walks are limited to 10 pages.

Service Binding callers use `GET /api/convert?url=…` or
`GET /api/browse?resource=profile|search|followers|following&…`. Short routes
`/:handle/status/:id`, `/:handle`, `/search`, `/:handle/followers`, and
`/:handle/following` work too. Responses default to Markdown; `format=json`
or `Accept: application/json` selects structured output. Post conversion also
supports `format=obsidian`. `HEAD` returns the same headers without a body.

FxTwitter is the direct public provider, with X syndication as the post fallback.
Successful results use the Workers Cache API for one hour; `nocache=true`
bypasses it. Failures are never cached. Requests have a 25-second deadline,
bounded provider response bodies, strict inputs, and no forwarded account
credentials. Rate limits return `429` and `Retry-After`; provider search outages
return `503` rather than an empty result. The tool preserves `http_status` and
`retry_after` on failures. Returned Markdown is untrusted source data.

Availability depends on upstream public providers. Search supports `latest`
and `top`. The separate authenticated session backend, Photos/Videos/Users
search feeds, optional paid providers, public lists, account mutations, preview
embeds, landing page, and administrative endpoints from x.md are not included.

From the repository root:

```sh
pnpm --filter @nanocodex/x-api run typecheck
pnpm --filter @nanocodex/x-api run test
pnpm --filter @nanocodex/x-api run build
pnpm deploy:x
pnpm deploy:managed
```

Deploy X before managed agents and account last. Root `pnpm dev` includes this
Worker in the account app's Vite auxiliary Workers. To exercise the Worker alone:

```sh
pnpm --filter @nanocodex/x-api run dev
curl 'http://localhost:8787/jack/status/20?format=json&thread=off'
```

Conversion, provider normalization, context ordering, and Markdown rendering
are adapted from [pc-style/x-md](https://github.com/pc-style/x-md) at commit
`5bc91d3f3a4ff0c0cf4e481b689a673ccbdb183f`. Its MIT notice is retained in
[`LICENSE.x-md`](LICENSE.x-md). The adaptation replaces Vercel/Bun/disk-cache
integration with Web APIs, Workers Cache, request cancellation, bounded
provider transport, and the native Nanocodex tool contract.
