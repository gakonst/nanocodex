# Nanocodex Chief of Staff

An independently deployable [Vercel Chat SDK](https://chat-sdk.dev/) Worker. The first
complete channel is Slack: the Worker verifies Slack signatures and timestamps,
fences one configured workspace, persists Chat SDK replay/subscription state, maps
each account/workspace/conversation to a Durable Object, and routes turns to one
retained managed Nanocodex agent.

The account app receives only readiness metadata through a service binding. It does
not receive provider credentials or the Nanocodex API key.

## Configure Slack

Create a single-workspace Slack app following the current official
[Slack adapter guide](https://chat-sdk.dev/adapters/slack). Enable `agent_view`, add
the `assistant:write`, `app_mentions:read`, `chat:write`, `channels:history`,
`groups:history`, `im:history`, `mpim:history`, and `users:read` bot scopes, and
subscribe to `app_mention`, `message.channels`, `message.groups`, `message.im`, `message.mpim`,
`app_home_opened`, `app_context_changed`, `agent_session_stopped`, and
`agent_session_title_changed`. Use the deployed `/webhooks/slack` URL as the Events
API request URL.

Configure every credential as a Worker secret:

```sh
pnpm exec wrangler secret put NANOCODEX_API_KEY --config js/chief-of-staff/wrangler.jsonc
pnpm exec wrangler secret put SLACK_BOT_TOKEN --config js/chief-of-staff/wrangler.jsonc
pnpm exec wrangler secret put SLACK_SIGNING_SECRET --config js/chief-of-staff/wrangler.jsonc
pnpm exec wrangler secret put SLACK_BOT_USER_ID --config js/chief-of-staff/wrangler.jsonc
pnpm exec wrangler secret put SLACK_TEAM_ID --config js/chief-of-staff/wrangler.jsonc
```

`NANOCODEX_API_KEY` binds the deployment to its owning Nanocodex account. The
readiness endpoint reports Slack ready only when the signed-in account matches that
owner. Set `CHIEF_OF_STAFF_PUBLIC_ORIGIN` in Wrangler configuration when deploying
under another Worker name or custom domain.

Deploy the managed service first, then this Worker, and the account application
last:

```sh
pnpm deploy:managed
pnpm deploy:chief-of-staff
pnpm deploy:account
```

## Capability boundary

- Slack uses the first-party `@chat-adapter/slack` contract and is implemented.
- WhatsApp has a first-party Chat SDK adapter, but this deployment does not expose
  or claim a Meta webhook yet.
- iMessage is listed by Chat SDK through vendor adapters, not a first-party adapter;
  this deployment does not configure one.

Run `pnpm --filter @nanocodex/chief-of-staff check` for signature, workspace fence,
idempotency, cross-account/channel isolation, durable two-turn, type, and dry-run
deployment coverage.
