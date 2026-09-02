# Nanocodex Chief of Staff

An independently deployable [Vercel Chat SDK](https://chat-sdk.dev/) Worker. The
Chief of Staff is a Slack AI bot installed into a workspace, like Devin. It is
not the Nanocodex Slack connector: the connector authorizes actions as a human
user, while this app has its own bot identity, OAuth installation, scopes,
tokens, event ingress, and lifecycle.

An authenticated Nanocodex user selects **Add to Slack** once. Slack presents the
workspace approval and returns to the account app. The Worker exchanges the code,
encrypts the workspace bot token in Durable Object state, records the workspace
installation, and immediately accepts signed mentions and DMs. Users never copy a
token or configure a webhook.

Each account/workspace/conversation maps to a retained managed Nanocodex agent.
The account app receives only non-secret installation and readiness metadata
through a service binding.

## Configure the shared Slack app

Create one distributable Slack app from `slack-app-manifest.yaml`, following the
official [Slack adapter guide](https://chat-sdk.dev/adapters/slack). Enable `agent_view`, add
the `assistant:write`, `app_mentions:read`, `chat:write`, `channels:history`,
`groups:history`, `im:history`, `mpim:history`, and `users:read` bot scopes, and
subscribe to `app_mention`, `message.channels`, `message.groups`, `message.im`,
`message.mpim`, `app_home_opened`, `app_context_changed`,
`app_uninstalled`, `agent_session_stopped`, and `agent_session_title_changed`.

Configure these URLs in Slack:

```text
OAuth redirect: https://nanocodex-chief-of-staff.gakonst.workers.dev/v1/slack/callback
Events request: https://nanocodex-chief-of-staff.gakonst.workers.dev/webhooks/slack
```

Configure the deployment once. `SLACK_ENCRYPTION_KEY` and
`SLACK_OAUTH_STATE_SECRET` are independent base64url-encoded 32-byte keys.

```sh
pnpm exec wrangler secret put NANOCODEX_API_KEY --config js/chief-of-staff/wrangler.jsonc
pnpm exec wrangler secret put SLACK_CLIENT_ID --config js/chief-of-staff/wrangler.jsonc
pnpm exec wrangler secret put SLACK_CLIENT_SECRET --config js/chief-of-staff/wrangler.jsonc
pnpm exec wrangler secret put SLACK_SIGNING_SECRET --config js/chief-of-staff/wrangler.jsonc
pnpm exec wrangler secret put SLACK_ENCRYPTION_KEY --config js/chief-of-staff/wrangler.jsonc
pnpm exec wrangler secret put SLACK_OAUTH_STATE_SECRET --config js/chief-of-staff/wrangler.jsonc
```

`NANOCODEX_API_KEY` binds this deployment to its owning Nanocodex account. Slack
OAuth installations are accepted only when initiated by that signed-in owner.
Set both public origins in Wrangler when deploying under other names or domains.

Deploy the managed service first, then this Worker, and the account application:

```sh
pnpm deploy:managed
pnpm deploy:chief-of-staff
pnpm deploy:account
```

## Capability boundary

- Chief of Staff: workspace-installed Slack app; bot token and bot scopes; reacts
  to bot DMs, mentions, and subscribed threads.
- Slack connector: separately authorized user token; acts on behalf of the human
  in only the exact workspaces granted through Connect.
- WhatsApp has a first-party Chat SDK adapter, but this deployment does not expose
  or claim a Meta webhook yet.
- iMessage is listed by Chat SDK through vendor adapters, not a first-party adapter;
  this deployment does not configure one.

Run `pnpm --filter @nanocodex/chief-of-staff check` for OAuth state, signature,
workspace fencing, idempotency, cross-account/channel isolation, durable two-turn,
type, and dry-run deployment coverage.
