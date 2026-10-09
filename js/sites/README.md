# Nanocodex Sites

`nanocodex-sites` serves static sites published from managed threads. Each site
lives at `<label>.<SITES_DOMAIN>`, or at `/<label>/` on the Worker's own host
when `SITES_DOMAIN` is empty. The label is a 26-character random value. The
Worker reads `hosts/<label>.json`, then the version manifest and the file, from
the `SITES` R2 bucket. It serves only `GET` and `HEAD` and revalidates every
response, so revoking a link takes effect on the next request.

Public links need only the URL. Previews are owner-only: the Worker redeems
their single-use grant once, sets a site-scoped session cookie, and requires
that cookie on every request after that.

The managed Worker creates and deletes host records. The only thing this Worker
writes is the session hash it adds to a view record when it redeems a grant. `src/format.ts`
(`@nanocodex/sites/format`) defines the shared object layout and parsers.

See [docs/SITES.md](../../docs/SITES.md) for the API, isolation model, and
deployment steps. Deploy with `pnpm deploy:sites`.
