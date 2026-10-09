# Static sites

A managed thread can publish a directory or file it generated, such as a build
output, a report page, or an image, as an immutable **site version**. The owner
can open a private preview or create public links. Sites are static files only;
there is no server code.

Publishing is private. A version becomes reachable only through a link the owner
creates, and each link serves exactly the version it was created for. Later
publishes never change what an existing link shows.

## Architecture

```
owner ──/v1/agents/:id/sites──▶ account Worker ──NANOCODEX_BACKEND──▶ managed Worker
                                                                        │ thread Durable Object
                                         reads /workspace or /brain ◀───┤ (sites, versions, hosts)
                                                                        ▼
anyone with link ──<label>.<zone>──▶ nanocodex-sites Worker ──reads──▶ R2 nanocodex-sites
```

- **Publishing** runs in the thread's Durable Object. It reads Cloudflare
  workspaces straight from their R2 prefix (`sessions/<resource>/`) and `/brain`
  through the brain workspace, so it never wakes a sandbox or runs model code.
  Files are content-addressed under `threads/<thread>/blobs/<sha256>`, and the
  version manifest under `threads/<thread>/manifests/<sha256>.json`.
- **Links** are host records at `hosts/<label>.json`, where the label is a
  26-character random DNS label (130 bits). The Durable Object is the only
  writer; deleting the record revokes the link.
- **Serving** is the `nanocodex-sites` Worker ([js/sites](../js/sites/README.md)).
  It reads the host record on every request, then the manifest and file. Every
  response is revalidated (`cache-control: no-cache` with a content ETag), so
  revocation and expiry apply to the next request.

The format both sides share lives in `@nanocodex/sites/format`.

## Isolation and response policy

Sites are served only from first-level subdomains of a dedicated registrable
zone, never the app's origin. Each site therefore has its own origin, can't read
the app's cookies, and can't read another site's storage. The zone should be on
the Public Suffix List.

Every site response carries:

- a CSP that allows scripts and styles from the site and common CDNs, but
  limits `connect-src`, `form-action`, and `base-uri` to the site itself and
  blocks plugins;
- `x-content-type-options: nosniff`, with each file's type fixed at publish time;
- `referrer-policy: no-referrer`, `x-robots-tag: noindex, nofollow`, and a
  restrictive `permissions-policy`.

The Sites Worker never sets cookies. Missing, revoked, and expired links return
the same 404 page.

## What is published

`path` is an absolute thread path:

| Path | Source |
| --- | --- |
| `/workspace/...` | The thread's Cloudflare workspace (the only one, or the legacy sandbox). |
| `/cloudflare-<name>/...` | A specific Cloudflare workspace mount. |
| `/brain/...` | The thread's shared `/brain`, including `/brain/outputs`. |

Other Hands (Mac, VM) are not yet supported publish sources.

A directory publishes its files recursively. `index.html` is served at `/`
unless `entry` names another file. A single file is served at `/`. With
`spa: true`, unknown extensionless paths serve the entry for client-side
routing. Requesting `/docs` when `docs/index.html` exists redirects to `/docs/`.

These are never uploaded, and are reported only as a count:

- the directories `.git`, `node_modules`, `.ssh`, `.aws`, `.gnupg`, and `.nanocodex`;
- `.env` and `.env.*`, `.npmrc`, `.netrc`, `.pypirc`, `.git-credentials`, SSH
  key files, and `.pem`, `.key`, `.p12`, `.pfx`, `.jks`, and `.keystore` files.

Limits: 2,000 files and 50 MB per version, 25 MB per file, 100 sites per
thread, and 50 active links per thread. Publishing content identical to the
latest version returns that version rather than creating a new one, so a
replayed tool call is idempotent.

## HTTP API

All routes require the thread owner (an account session or API key, not a
Connect grant). Reads need `agents:read`; changes need `agents:write`;
publishing also needs `tools:use`. Cookie-authenticated changes must be
same-origin.

| Method | Route | Does |
| --- | --- | --- |
| `GET` | `/v1/agents/:id/sites` | List sites with their versions and active links. |
| `POST` | `/v1/agents/:id/sites` | Publish `{ path, id?, title?, entry?, spa? }`. Returns `201` for a new version, `200` for an identical one. |
| `POST` | `/v1/agents/:id/sites/:site/open` | Mint a private host for `{ version? }` that works for one hour. |
| `GET` | `/v1/agents/:id/sites/:site/shares` | List active links. |
| `POST` | `/v1/agents/:id/sites/:site/shares` | Create a link for `{ version?, expires_at? }`. `version` defaults to the latest. |
| `DELETE` | `/v1/agents/:id/sites/:site/shares/:share` | Revoke a link. |

```sh
curl -X POST "$NANOCODEX_URL/v1/agents/$AGENT/sites" \
  -H "authorization: Bearer $NANOCODEX_API_KEY" -H 'content-type: application/json' \
  -d '{"path":"/workspace/app/dist","id":"launch","title":"Launch page"}'
curl -X POST "$NANOCODEX_URL/v1/agents/$AGENT/sites/launch/shares" \
  -H "authorization: Bearer $NANOCODEX_API_KEY" -H 'content-type: application/json' -d '{}'
```

Errors are JSON `{ error, message }`: for example `site_source_not_found` (404),
`site_source_excluded` and `site_entry_required` (422), `site_too_large` (413),
`site_limit` and `site_share_limit` (429), and `sites_unavailable` (503) when
the bucket or link origin isn't configured.

## Agent tools and clients

- `publish_site` publishes a version and returns
  `{ type: "nanocodex.site", site_id, title, version, entry, files, bytes, excluded, created }`.
- `site_sharing` lists, creates, and revokes links. It creates a link only when
  the user explicitly asks, never retries an uncertain create, and lists links
  without URLs.
- Both tools are limited to the direct account root agent. Connect grants,
  thread-share guests, and subagents can't use them.
- A link that appears in the conversation, such as the result of a
  `site_sharing` create, is visible to anyone the thread is shared with.
- The web app renders a card for each `publish_site` result, with **Open
  preview**, **Create public link**, and **Turn off**.
- The `nanocodex2` TUI provides `/sites`, `/sites publish <path> [id]`,
  `/sites open <id> [version]`, `/sites share <id> [version]`, and
  `/sites revoke <id> <link-id>`.

Deleting a thread first deletes all of its host records, so its links stop
resolving before slower cleanup, and then deletes its objects.

## Operations

1. Create the R2 bucket `nanocodex-sites` before deploying managed.
2. Choose a dedicated zone. Add a proxied wildcard DNS record and attach
   `*.<zone>/*` to `nanocodex-sites` (see `js/sites/wrangler.jsonc`), then set
   its `SITES_DOMAIN` to the zone.
3. Set managed's `NANOCODEX_SITES_ORIGIN` to `https://*.<zone>`.
4. Deploy with `pnpm deploy:sites`. CI releases it in the first phase, with
   the X and media Workers.

Without steps 2 and 3, publishing works, but opening and sharing return
`sites_unavailable`.

## Verification

```sh
pnpm --filter nanocodex-managed-service run prepare:code-evaluator
pnpm --filter nanocodex-managed-service run test:sites
cargo build -p nanocodex2-bin --bin nanocodex2 && node bin/nanocodex/tests/sites-tui-e2e.mjs
```

The first command runs the real account proxy, managed Worker, thread SQLite,
R2, and Sites Worker in Miniflare, and requests every site URL with `curl`. It
writes `output/sites-journey.json`. The TUI journey drives `/sites` in a PTY
against a fixture transport and writes `output/sites-tui/trace.json`.
