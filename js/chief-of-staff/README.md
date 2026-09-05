# Nanocodex Chief of Staff

An independently deployable [Vercel Chat SDK](https://chat-sdk.dev/) Worker. The
Chief of Staff is a Slack, WhatsApp, and Viber AI assistant backed by durable
Nanocodex agents. It is not the Nanocodex Slack connector: the connector
authorizes actions as a human user, while these channels have their own
bot/business identity, credentials, signed event ingress, and lifecycle.

An authenticated Nanocodex user selects **Add to Slack** once. Slack presents the
workspace approval and returns to the account app. The Worker exchanges the code,
encrypts the workspace bot token in Durable Object state, records the workspace
installation, and immediately accepts signed mentions and DMs. Users never copy a
token or configure a webhook.

Each verified provider user maps to a private, persistent Nanocodex account and
each conversation maps to one retained managed agent. The Chief Worker reaches
the managed Worker through the capability-scoped `ChiefOfStaffBackend` service
binding; it holds no Nanocodex bearer credential and cannot select a Nanocodex
account ID. The account app receives only non-secret installation and readiness
metadata.

The egress Worker funds these generated accounts with one operator-owned OpenAI
credential. It remains encrypted inside egress credential storage and never
crosses the Chief RPC or browser boundary:

```sh
pnpm --filter nanocodex-egress-service exec wrangler secret put CHIEF_OF_STAFF_OPENAI_API_KEY --config wrangler.broker.jsonc
```

The same Worker also exposes an official Viber Bot REST API channel. It verifies
the raw callback HMAC, maps each bot/subscriber pair to a durable conversation,
and returns the retained managed agent's reply through the configured Viber bot.

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
pnpm --filter @nanocodex/chief-of-staff exec wrangler secret put SLACK_CLIENT_ID --config wrangler.jsonc
pnpm --filter @nanocodex/chief-of-staff exec wrangler secret put SLACK_CLIENT_SECRET --config wrangler.jsonc
pnpm --filter @nanocodex/chief-of-staff exec wrangler secret put SLACK_SIGNING_SECRET --config wrangler.jsonc
pnpm --filter @nanocodex/chief-of-staff exec wrangler secret put SLACK_ENCRYPTION_KEY --config wrangler.jsonc
pnpm --filter @nanocodex/chief-of-staff exec wrangler secret put SLACK_OAUTH_STATE_SECRET --config wrangler.jsonc
```

## Configure Viber

Provision a commercial chatbot directly with Rakuten Viber or an official messaging
partner. Copy the bot token, display name, and stable bot URI into Worker secrets:

```sh
pnpm --filter @nanocodex/chief-of-staff exec wrangler secret put VIBER_AUTH_TOKEN --config wrangler.jsonc
pnpm --filter @nanocodex/chief-of-staff exec wrangler secret put VIBER_BOT_NAME --config wrangler.jsonc
pnpm --filter @nanocodex/chief-of-staff exec wrangler secret put VIBER_BOT_URI --config wrangler.jsonc
# Optional
pnpm --filter @nanocodex/chief-of-staff exec wrangler secret put VIBER_BOT_AVATAR --config wrangler.jsonc
```

`VIBER_BOT_NAME` must be at most 28 characters. `VIBER_BOT_URI` is the stable URI
returned by Viber's `get_account_info` API; it keeps durable conversations attached
to the same bot if its authentication token rotates. `VIBER_BOT_AVATAR` is an
optional HTTPS avatar URL.

Register `https://<chief-of-staff-origin>/webhooks/viber` with Viber's
`set_webhook` endpoint. The Worker accepts only callbacks carrying a valid
`X-Viber-Content-Signature`. Opening the bot produces a short welcome message;
the first user message subscribes the user and creates their durable agent session.
Text, image, video, file, URL, sticker, location, and contact inputs are normalized
into turns. Replies respect Viber's 7,000-character text limit. A durable delivery
claim suppresses duplicate outbound messages when Viber retries an already handled
callback; expired claims are recoverable after an interrupted send.

Slack OAuth installation metadata remains owned by the signed-in Nanocodex user
who installed it, so only that user can list or remove the installation. Runtime
Slack actors are intentionally separate: `(team_id, event.user)` identifies the
actor's managed account, including in shared channels. Viber uses `(bot URI,
subscriber ID)` and WhatsApp uses `(phone-number ID, user ID)`. These mappings are
persistent; uninstalling a provider stops new ingress but does not currently
delete retained Nanocodex data. Set both public origins in Wrangler when deploying
under other names or domains.

## Configure WhatsApp

Follow the official [WhatsApp adapter guide](https://chat-sdk.dev/adapters/official/whatsapp)
to add WhatsApp to a Meta business app and connect its production business phone
number. Configure this callback URL and use the same verify token stored in the
Worker:

```text
Callback: https://nanocodex-chief-of-staff.gakonst.workers.dev/webhooks/whatsapp
Webhook fields: messages, user_id_update
```

Use a permanent System User token in production. Keep the access token, app
secret, and verify token as Worker secrets. The business phone ID is a
non-secret routing identifier, but this deployment also stores it out of band;
none of these values belong in the account app or browser environment.

```sh
pnpm --filter @nanocodex/chief-of-staff exec wrangler secret put WHATSAPP_ACCESS_TOKEN --config wrangler.jsonc
pnpm --filter @nanocodex/chief-of-staff exec wrangler secret put WHATSAPP_APP_SECRET --config wrangler.jsonc
pnpm --filter @nanocodex/chief-of-staff exec wrangler secret put WHATSAPP_PHONE_NUMBER_ID --config wrangler.jsonc
pnpm --filter @nanocodex/chief-of-staff exec wrangler secret put WHATSAPP_VERIFY_TOKEN --config wrangler.jsonc
```

Meta verifies `GET /webhooks/whatsapp` with the verify token. The official
adapter verifies `X-Hub-Signature-256` on every POST before the message can reach
the user's isolated agent. Reactive replies are sent inside WhatsApp's 24-hour
customer-service window; initiating a later conversation requires an approved
message template.

Deploy egress first, then the managed service, this Worker, and the account
application:

```sh
pnpm deploy:egress
pnpm deploy:managed
pnpm deploy:chief-of-staff
pnpm deploy:account
```

## Capability boundary

- Chief of Staff on Slack: workspace-installed app; bot token and bot scopes;
  reacts to bot DMs, mentions, and subscribed threads.
- Chief of Staff on WhatsApp: Meta Cloud API business number; signed DMs and
  media placeholders; replies to the exact canonical WhatsApp user route.
- Slack connector: separately authorized user token; acts on behalf of the human
  in only the exact workspaces granted through Connect.
- Viber: official Bot REST API; branded bot identity; each subscriber receives an
  isolated durable conversation.
- iMessage is listed by Chat SDK through vendor adapters, not a first-party adapter;
  this deployment does not configure one.

Run `pnpm --filter @nanocodex/chief-of-staff check` for OAuth state, Slack,
WhatsApp, and Viber signatures, workspace fencing, idempotency,
cross-provider/user/channel isolation, durable two-turn, type, and dry-run deployment
coverage.
