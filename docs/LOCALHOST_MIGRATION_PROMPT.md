# Localhost development migration

Normal Nanocodex development uses the reserved `.localhost` domain directly.
OrbStack, Docker, local TLS, and `.local` DNS are not part of the required
application, Connect, passkey, CLI, or OAuth path.

## Canonical topology

`cd web && npm run dev` starts one isolated Cloudflare/Vite stack at:

```text
http://<instance>.nanocodex.localhost:<instance-port>
http://playground-<instance>.nanocodex.localhost:<instance-port>
```

The primary checkout uses `nanocodex.localhost`; linked worktrees get a stable
single-label instance hostname and deterministic port. Every instance has its
own Wrangler/Miniflare state directory. All instances use WebAuthn RP ID
`nanocodex.localhost` and the same development-only passkey portability HMAC
key loaded through the main-checkout environment.

Passkey registration/authentication writes a signed, HttpOnly parent-domain
record containing only credential ID, public key, and Nanocodex user ID. A
fresh worktree verifies that record before seeding its isolated WebAuthn store.
Exact browser origin, challenge, RP ID, credential ID, and assertion signature
checks remain mandatory. A `.local` passkey is a different RP credential and
must be recreated once on `.localhost`.

## Fixed local OAuth relay

Development OAuth applications register the fixed connector callbacks below.
Dynamically registered remote-MCP clients use the final connection-ID route:

```text
http://127.0.0.1:47891/v1/connectors/github/callback
http://127.0.0.1:47891/v1/connectors/gmail/callback
http://127.0.0.1:47891/v1/connectors/gdrive/callback
http://127.0.0.1:47891/v1/connectors/x/callback
http://127.0.0.1:47891/v1/connectors/slack/callback
http://127.0.0.1:47891/v1/mcp-connections/<opaque-connection-id>/callback
```

The fixed relay binds only the loopback interface at `127.0.0.1:47891`, while
its provider-facing callback identity is `http://127.0.0.1:47891`. It is a standalone stateless
process, not a Worker/Miniflare service. The first development stack starts it;
concurrent stacks authenticate and adopt the same daemon, which is deliberately
independent of any one worktree lifecycle.

Connector and remote-MCP start wrap the broker's opaque OAuth state in a ten-minute
HMAC-SHA256 routing envelope. The OAuth relay key is distinct from the passkey
portability key. The envelope binds the provider or opaque MCP connection ID,
exact initiating origin, flow,
original state, issue/expiry time, and nonce. Only root or one-label
`*.nanocodex.localhost:<port>` HTTP origins are accepted. The relay restores
only the original state and provider code/error fields onto a fixed internal
callback path. Provider token exchange and PKCE verification remain in the
private egress broker; provider secrets and tokens never enter browser storage
or the CLI.

Do not register worktree origins, wildcard callbacks, `.local`, or `localhost`
aliases with providers. Port `47891` is reserved for the relay; application
instances must not use it.

## Completion checks

- Start two isolated stacks concurrently without OrbStack or Docker.
- Create a `.localhost` passkey in instance A and use it in instance B.
- Reload both instances and prove there is no unknown-credential or
  identity-mismatch loop.
- Verify “Use another passkey” clears only the signed portable hint and invokes
  authenticator account selection.
- Start Account and Connect connector flows and inspect the real provider URLs;
  every `redirect_uri` must use `127.0.0.1:47891`.
- Complete configured provider callbacks, disconnect, and reconnect.
- Run `nanocodex login`, `nanocodex connect github`, and a real nanocodex2 agent
  turn through the resulting grant.
- Inspect browser network/storage and prove provider credentials are absent.
- Exercise desktop and representative mobile layouts.

Production hosted routing is unchanged.
