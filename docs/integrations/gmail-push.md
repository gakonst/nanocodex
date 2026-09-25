# Continuous Gmail notifications

Gmail `users.watch` publishes mailbox history hints to Cloud Pub/Sub. The account
Worker exposes `POST /v1/gmail-push/{userId}/{connectionId}` and forwards only that
fixed path to egress. Egress verifies Google's RS256 signature, issuer, expiration,
audience, verified email and configured push service account. The mailbox Durable
Object checks the connected Gmail profile, stores a history cursor and durable
outbox, and renews the watch daily. OAuth tokens stay in UserConnectorBroker.

## Provisioning

Enable Gmail API and Pub/Sub in the **same Google project as the configured OAuth
client**. Create a topic and grant `gmail-api-push@system.gserviceaccount.com`
`roles/pubsub.publisher` on that topic. Configure these non-secret egress variables:

- `GMAIL_PUSH_TOPIC`: `projects/example-project/topics/gmail-events`
- `GMAIL_PUSH_AUDIENCE`: the fixed audience configured on every push subscription,
  for example `https://app.example/v1/gmail-push`
- `GMAIL_PUSH_SERVICE_ACCOUNT`: the dedicated push identity's service-account email
- `GMAIL_PUSH_SUBSCRIPTION`: exact `projects/.../subscriptions/...` allowlist
- `GMAIL_PUSH_OWNER_ID` and `GMAIL_PUSH_CONNECTION_ID`: the single enabled account
  and exact Google connection; all other mailbox configurations and push paths are denied

Create a wrapped, authenticated push subscription for the configured account/connection
on the topic. This receiver configuration admits one exact subscription. Its endpoint is
`https://app.example/v1/gmail-push/{userId}/{connectionId}`. Set its OIDC service
account and audience to the configured values. Grant Pub/Sub's service agent the
required `iam.serviceAccounts.getOpenIdToken` permission on the push identity.
Keep payload unwrapping disabled. The identity should be dedicated to this
integration; requests from other identities are rejected. Events for other mailbox email addresses are acknowledged without waking an agent.
This first integration supports one mailbox per deployment. Multi-mailbox delivery
requires a separate routing design; changing these variables does not migrate watches.

Deploy the egress DO migration and managed wake receiver, then the account Worker.
This source change does not provision Google resources or deploy any Worker.
The existing Google connection needs Gmail read access; reconnect through normal
OAuth consent if it only grants other Workspace capabilities.

## Account API

Use the authenticated account API to configure a mailbox's target agent:

```
PUT /v1/agents/{agentId}/gmail-push/{connectionId}
GET /v1/agents/{agentId}/gmail-push/{connectionId}
DELETE /v1/agents/{agentId}/gmail-push/{connectionId}
```

The agent must belong to the authenticated account. Connect grants cannot create
these watches. PUT takes a JSON body `{"email":"mailbox@example.com"}`. The server verifies
that address against the exact connection's Gmail profile. One mailbox has one target agent; disable before changing it. Enable only
after the Pub/Sub subscription exists, then send a test message and inspect status
and the resulting agent turn before disabling the old five-minute polling cron.
The integration does not automatically remove existing account schedules.

The provisioning helper defaults to an offline dry run:

```sh
node scripts/gmail-push-setup.mjs --project example-project \
  --gmail-oauth-project example-project \
  --push-endpoint https://app.example/v1/gmail-push/OWNER/CONNECTION
```

Inspect its output before using `--apply`. It provisions Google resources only;
configure the account API separately.

## Delivery and recovery

Push acknowledgment means the history target is durable, not that an agent turn
finished. Duplicate and older history hints do not create duplicate work. Alarms
page through Gmail history and persist an outbox before admitting a turn. A busy
agent or an unavailable provider causes bounded retry; replay uses the same event
ID so an uncertain admission response cannot start a second turn. Work per alarm
and the message IDs included in each event are bounded. Renewal catches up from
the existing cursor; it never replaces an unprocessed cursor with the watch result.
An hourly backend-only history check recovers missed pushes; empty changes do not
wake the agent. Each alarm admits at most one turn with up to 100 message IDs;
remaining chunks stay durable and retry after the agent becomes idle.
An expired history cursor produces an explicit resynchronization event rather
than silently claiming all intervening changes were delivered.

Only newly added INBOX messages wake the agent. Draft, sent-only and label-only
changes do not trigger turns. Self-addressed mail delivered to INBOX remains eligible. Events carry message identifiers and history metadata, not email bodies. They are
untrusted context. Receiving an event does not authorize sending email or other
external actions; existing explicit user authorization is still required.
Disabling stops local wakes immediately and attempts `users.stop`; inspect the
returned `watchStopped` value. Delete the associated Pub/Sub subscription when
retiring a mailbox. Gmail's watch is shared per mailbox/project, so another client
using the same OAuth project can replace or stop it.

See Google's [Gmail push guide](https://developers.google.com/workspace/gmail/api/guides/push)
and [authenticated Pub/Sub push documentation](https://docs.cloud.google.com/pubsub/docs/authenticate-push-subscriptions).
