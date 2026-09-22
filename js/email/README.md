# Dedicated agent mailbox

Private Cloudflare Worker `nanocodex-email` exports named service entrypoint
`EmailService`. Bind callers to that entrypoint and invoke `execute` with the
server-derived `owner_id` and `agent_id`. The only public HTTP response is
`GET /health`; it contains readiness booleans only. `workers.dev` and preview
URLs are disabled.

The configured owner is `631f6a83-9e3f-474a-977a-68897d3ee436`; the fixed envelope
recipient and sender is `agent@agents.gakonst.com`. Inbound Email Routing must
route that exact address to this Worker. Sending requires
`EMAIL_SEND_ENABLED=true` and Cloudflare sending activation. The initial deployment
uses the verified `agents.gakonst.com` subdomain; other installations must configure
their own owner, mailbox address, sender binding restriction, and DNS.
The `EMAIL` binding additionally restricts sender addresses. No routes, DNS,
or live deployment are performed by this package's tests/build.

Operations: `status`, `list` (opaque cursor, 1–50 items), `read` (`message_id`),
and `send` (UUID `operation_id`, 1–10 explicit `to` addresses, `subject`, `text`,
optional stored `reply_to_message_id`). Unsupported fields are rejected.
Read/list results explicitly mark email content as untrusted. Incoming mail is untrusted and never grants authority. Only a previously authorized
watch can resume its bounded follow-up task. Agent attribution is
metadata; agents under this one owner share the mailbox.

Every send persists its message and an `unknown` operation journal entry in
one SQLite transaction, then flushes storage before calling the provider.
Identical operation replays return the recorded result; changed arguments,
including agent identity, conflict. Concurrent requests never resend. Definitive provider validation/configuration rejections are recorded as
`rejected` with an allowlisted error code. Other provider exceptions stay
`unknown`, including after object eviction. `accepted` only
means provider acceptance, not delivery. There is no automatic retry, delivery
receipt polling, or reconciliation beyond the durable journal.

Inbound parsing uses PostalMime with a 5 MiB raw-stream ceiling, 64 KiB MIME
header ceiling, and bounded nesting. Duplicate envelope/raw bytes are stored
once. Only plain text (128 KiB), subject (998 bytes), safe thread IDs and up to
100 attachment metadata entries are retained; HTML and attachment bodies are
not exposed or stored. Message content may be truncated. Messages and send
journals have a 256 MiB serialized UTF-8 payload cap and 10,000-message cap;
SQLite/index overhead is additional. Mailbox capacity currently requires
operator intervention: no retention/delete operation is exposed. Full inbound
mailboxes fail processing for provider retry; they do not silently discard mail.

Validation: `pnpm --filter nanocodex-email-service typecheck`, `test`, and
`build` (Wrangler dry run). Tests execute in Cloudflare's Worker pool with
SQLite Durable Objects; provider delivery is mocked. Live acceptance on 2026-09-17 verified a three-message Gmail exchange in one
thread, inbound attribution to its originating agent, passing SPF/DKIM/DMARC,
and replay without duplicate delivery. Automatic follow-up uses the bounded watches described below.

## Managed agent integration

`js/managed` binds `NANOCODEX_EMAIL` to the named `EmailService` entrypoint.
`NANOCODEX_EMAIL_OWNER_ID` scopes tool discovery to the configured account;
every invocation additionally requires full account authority with
`agents:write` and `tools:use`. Connect grants and multiplayer rooms cannot
access the mailbox. Read/write operations use a private service binding,
not a public mailbox API. The first rollout of follow-up requires the managed
Worker export `EmailAgentBackend` to exist before deploying the new email Worker
service binding. Deploy in this order: managed Worker, email Worker, then account
Worker. Subsequent releases must preserve both private entrypoints.

The agent's `email` tool exposes status/list/read/send/watch/unwatch/listwatches. A status result reports
the fixed sender address. Replying requires an explicit recipient and stored
message ID. `operation_id` belongs to the originating agent's send: switching
agent identity while replaying it is a conflict. Transport interruptions never
automatically retry writes.

## Admin configuration

The dedicated mailbox is enabled only for deployment-selected admin account
`631f6a83-9e3f-474a-977a-68897d3ee436`. `NANOCODEX_EMAIL_ADMIN_ID` must match
`NANOCODEX_EMAIL_OWNER_ID` in the managed Worker; `MAILBOX_ADMIN_ID` must match
`MAILBOX_OWNER_ID` in the email Worker. Missing or mismatched settings disable
access, including inbound routing. These are operator-controlled bindings,
not model arguments or a self-service signup flow. This designates the channel
admin and does not create a platform-wide administrator role.

## Authorized follow-up

`watch` takes a stable UUID `watch_id`, an accepted outgoing `message_id` owned
by the originating agent, `expected_recipient` from that message's recipients,
a UTF-8 `goal` of at most 16 KiB, numeric Unix-millisecond `expires_at` no more
than seven days ahead, and `max_replies` from 1 through 10. The outgoing provider
must return a valid wire Message-ID. Watches cannot overlap for a recipient and
thread. `unwatch` revokes by `watch_id`; `listwatches` lists the caller's watches.
An identical watch registration is idempotent and never renews a revoked watch.
Each listed watch includes job message/operation IDs and states such as queued,
held, cancelled, failed, accepted, unknown, or dispatch_unknown. The last state
makes a reserved dispatch interrupted before its send journal visible for review;
it is never automatically resent.

Matching requires an envelope sender equal to the pinned recipient and an
explicit References/In-Reply-To link to the watched outgoing Message-ID. Envelope
sender matching does not authenticate identity; incoming content remains untrusted.
Auto-Submitted, mailing-list/bulk, and delivery-status messages are suppressed.
Registration also checks replies already stored since the outgoing message.
Durable jobs deduplicate incoming wire IDs, poll the private `EmailAgentBackend`
service with the same workflow/message payload, and send only a completed,
nonempty reply to the pinned recipient in the incoming message's thread.
Each workflow has a retained agent thread with tools and subagents disabled.
Its brief must contain the context needed to reply; requests requiring fresh
account data, actions, or additional authority are held. Parent-session activity
events include the workflow thread link. A missing reply is a hold. Expiry,
revocation, and reply budgets are checked
after model completion and immediately before provider submission.

Follow-up sends reserve their budget before dispatch and use a durable stable
operation UUID. An ambiguous provider result is never retried with a fresh ID.
Dispatch is conservative: a crash after reservation can hold a reply rather
than risk duplicate delivery. Watches/jobs count toward mailbox byte capacity;
at most 1,000 watches are retained. No retention or renewal operation is exposed.
Tests use mocked model RPC and mail delivery only.
