# Connect API

## Fresh connector status for connected apps

`GET /v1/grants/:grantId/connectors?providers=spotify,soundcloud` returns:

- `account_id`, `agent_id`, and the current `grant` projection;
- `connectors`, containing only the requested API providers and their currently
  connected identities selected by this grant.

Send the grant's opaque bearer token, `x-nanocodex-app-id`, and the registered app
`Origin`, as with other grant routes. The endpoint validates the current grant,
expiry, app binding, and live connector identities on every request. Responses
are `no-store`; consumers must not cache the authorization decision.

`providers` is a required, comma-separated, non-duplicated list of API connector
capabilities. ChatGPT is excluded because its credential status lives in a
separate broker. This endpoint never reads Vault credentials, account balances,
or the account's authorization index. It returns no provider credentials or grant
bearer token. Use account-info when the full account summary is needed.

### Live output checkpoints

`GET /v1/grants/:grantId/agents/:agentId/checkpoints?turn_id=<id>&after=<revision>`
returns the newest complete intermediate output for one retained turn. It requires
`agent.output.final` plus either `agent.output.actions` or `agent.trace.read` on
an active grant. No new grant approval is needed. Cross-grant turns are not visible.

The agent writes immutable files under
`/brain/connect/<grantId>/outputs/<turnId>/checkpoints/r<N>/`, then atomically
writes `checkpoints/latest.json` last:

```json
{"revision":1,"files":[{"path":"r1/model.step","sha256":"<lowercase SHA-256>","size":123}]}
```

A manifest has 1–8 files, at most 1 MB each and 4 MB total. Names are bounded ASCII
basenames with no nested paths. The service verifies size and SHA-256 and atomically
retains one coherent bundle per turn. A partial, invalid, stale, or replayed revision
leaves the last validated bundle available. The JSON response includes `turn_id`,
`revision`, and each file's metadata plus `data_base64`. `204` means no validated
checkpoint yet; `304` means no revision newer than `after`. Responses are `no-store`.

Checkpoints survive observer disconnects and turn archival. They are intermediate
previews, not completion receipts. The final immutable artifact publication excludes
the `checkpoints` directory; agents must still publish the requested final outputs.
Session deletion removes retained snapshots and fences pending reads.
