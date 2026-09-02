# Nanocodex egress

This package contains the private Cloudflare credential broker and a small
service-binding example agent. Operational policy, deployment order, and
production evidence live in [../../AGENTS.md](../../AGENTS.md).

## Entrypoints

- `wrangler.broker.jsonc` deploys `src/egress.ts` as `nanocodex-egress`.
  It has `workers_dev = false` and no public routes; managed services reach it
  only through a Service Binding.
- `wrangler.agent.jsonc` deploys `src/agent.ts` as the public
  `nanocodex-egress-agent-example`. Its `EGRESS` binding demonstrates the
  private call shape; it is not the broker or a production control surface.

The package scripts expose broker and example-agent dry runs and deployments
(`dry-run:broker`, `deploy:broker`, `dry-run:agent`, and `deploy:agent`). Use
the repository operator interface in `../../AGENTS.md` for actual operation.

## Credential boundary

The broker owns per-user provider credentials, connector OAuth state, MCP
connection material, and brokered SSH private keys. Durable Objects encrypt
that state with AES-256-GCM before storage. Production requires
`CREDENTIAL_ENCRYPTION_KEY`; a static Secrets Store binding can supply it, and
`CREDENTIAL_ENCRYPTION_KEY_PREVIOUS` supports key rotation.

Credentials and encryption keys never enter browser code, managed Workers,
agent configuration, tool output, or status/control responses. The managed
account authenticates its own control request and supplies the resolved user
path; browser callers do not select users, subjects, upstreams, or credentials.
The development-only ChatGPT bootstrap claim is enabled only by the explicit
development/test environment and `ALLOW_LOCAL_CREDENTIAL_CLAIM=true`.

## Direct, fail-closed egress

`AgentSubjectDirectory` maps each opaque subject directly to one user. Binding,
unbinding, and resolution are private control operations; tombstones prevent a
deleted subject from being rebound. Managed code retains the subject, never a
credential or credential selector.

Model traffic accepts only the fixed internal URLs, methods, headers, and
credential placeholder. The broker resolves the subject, chooses that user's
active credential, injects it only for the approved upstream or configured
relay, and strips sensitive response headers. It rejects caller-selected
destinations, provider headers, redirects, and malformed WebSocket handshakes.

Connector, MCP, and SSH egress use the same subject boundary. Connector and
MCP requests are allowlisted and owner-checked. SSH accepts an opaque identity
reference and exact target, keeps the private key in the broker, verifies the
stored host fingerprint, and returns bounded command results.

OAuth connections use opaque 43-character base64url IDs and bounded labels.
Provider calls select an identity with
`X-Nanocodex-Connector-Connection`; a selector is required when more than one
eligible identity exists. The private control routes are:

- `GET /users/:user/connectors` for capability-projected status.
- `POST /users/:user/connectors/:provider` and the corresponding `/callback`
  route, where provider is `github`, `google`, `slack`, or `x`.
- `DELETE /users/:user/connectors/:provider/connections/:connectionId` for one
  exact grant.

Google is a control-only provider. One Google identity is projected under each
scope actually granted: `gmail`, `gdrive`, `gcalendar`, `gtasks`, `gdocs`,
`gsheets`, `gslides`, and read-only `gcontacts`. The legacy `gmail` and
`gdrive` control aliases remain readable during migration. Connector state and
all access/refresh tokens remain encrypted in the per-user credential vault;
status exposes only connection ID, label, account ID, and capability names.

Before enabling Slack in production, configure `SLACK_OAUTH_CLIENT_ID` and
`SLACK_OAUTH_CLIENT_SECRET` as secrets on the `nanocodex-egress` broker (never
on an application or managed Worker). The Slack app must register the canonical
`/v1/connectors/slack/callback` URL for the deployed Nanocodex origin and allow
the user scopes requested in `src/connectors/slack.ts`. Local development uses
the Vite-owned loopback relay instead of the production callback.

## Checks

`typecheck` and `test` cover this package. For a changed Worker boundary,
exercise the deployed flow and inspect browser/network, Worker logs, bindings,
and credential absence as required by `../../AGENTS.md`.
