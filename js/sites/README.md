# Nanocodex Sites

`nanocodex-sites` serves static sites published from managed threads. Each
request's hostname is `<label>.<SITES_DOMAIN>`, where the label is a 26-character
random capability. The Worker reads `hosts/<label>.json`, then the version
manifest and the file, from the `SITES` R2 bucket. It serves only `GET` and
`HEAD`, sets no cookies, and revalidates every response, so revoking a link
takes effect on the next request.

The managed Worker is the only writer of the bucket. `src/format.ts`
(`@nanocodex/sites/format`) defines the shared object layout and parsers.

See [docs/SITES.md](../../docs/SITES.md) for the API, isolation model, and
deployment steps. Deploy with `pnpm deploy:sites`. With an empty
`SITES_DOMAIN`, the Worker serves nothing.
