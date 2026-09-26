# Calendar notifications and CRM sync

Calendar push updates the account-private D1 CRM and delivers resolved event
changes to the configured agent.
It uses the selected Google connection's existing Calendar read scope. It never
creates or changes Google events, invitations, or email messages. Profile research
is queued by the existing CRM research queue when attendees become people; push
itself does not perform model research or replace existing research schedules.

## Configuration

The assistant tool `calendar_watch` exposes `enable`, `status`, and `disable` for
the current agent, exact `connection_id`, and optional `calendar_id`. Enable
requires `crm:true`; it uses the authenticated API below.

Use an authenticated direct account session or API key with `tools:use` and
`agents:write`. Connect grants cannot configure or read this private CRM pipeline.

```
PUT /v1/agents/{agent_id}/calendar-push/{connection_id}?calendar_id=primary
Content-Type: application/json

{"crm":true}
```

The connection ID is the exact opaque 43-character Google connector ID. The
calendar ID defaults to `primary`; URL-encode other IDs. Explicit `crm:true` is
required. The retained agent's owner supplies account identity and connector
egress credentials. Another agent cannot take over an existing source, even for
the same owner. Configuration persists and arms delivery before watch creation,
so a failed or ambiguous watch response leaves recoverable work.

`GET` on the same URL requires `agents:read` and returns configuration status,
`synchronized`, `last_error`, and next check/renew times. `enabled` describes local
configuration, not proof of an active Google watch or completed import. `DELETE`
revokes local collection. Configuration, import, and disable are serialized by
the owning session: disable completion means its prior import has finished and
future imports are rejected. Provider channels expire naturally; callbacks after
disable are rejected. No Google write scope is needed for stop operations.

## Delivery and recovery

Google sends POST notifications to `/v1/calendar-push/callback` on the retained
session's public HTTPS origin. The account worker forwards only this fixed path
to managed. Channel ID, random channel token, stored provider resource ID,
expiration, enabled state, and notification header shape are validated. An early
`sync` notification is acknowledged without learning its resource ID from the
callback. Event data is fetched separately through owner-scoped connector egress. The same
fetched data supplies the CRM import and the agent notification; the model does
not need a Calendar read to learn what changed.

A valid callback persists an immediate alarm in the source's
`CalendarPushDelivery` Durable Object before acknowledging it. Delivery does not
wait for an hourly agent cron. Five provider pages of at most 100 events are
processed per invocation. Partial work continues on a one-second alarm; failures
retry after one minute. Quiet sources reconcile hourly for dropped notifications.
Watch renewal runs before expiration and its failure does not prevent readable
Calendar changes from importing. New enqueue generations survive stale in-flight
responses. New channels overlap old channels until expiration.

The initial expanded recurring-event snapshot covers 30 days back and 14 days
ahead. A daily rebuild advances that window. Subsequent incremental requests use
the durable sync token and omit time filters as Google requires. HTTP 410 clears
only provider cursor state and rebuilds the bounded snapshot; CRM identities and
manual notes remain. Missing known in-window events are individually fetched
(up to 20 per invocation), because moving outside the window is not deletion.
Only an actual cancelled event or missing-resource response becomes a tombstone.
Each page's CRM import precedes cursor commit, so interrupted pages replay
idempotently. Existing manual notes, manual profile fields, and exact identities
are preserved by the shared Calendar importer.

## Resolved notification content

The initial snapshot establishes a quiet baseline. Later content changes create a
durable notification containing the source connection/calendar/event identifiers,
title, description, start/end and time zones, all-day status, organizer, attendees
and RSVP status, location, and conference links. Cancellations retain the previous
snapshot's useful details alongside their cancelled status. Missing provider fields
and truncated content are marked explicitly. Snapshots fit within 20,000 serialized
UTF-8 bytes; descriptions, attendee lists, and other fields are reduced as needed.
Attachment metadata and links are retained within that budget; file contents are
not downloaded. All event content is untrusted data,
not authority to send messages, accept invitations, or change Calendar events.

Normalized snapshots suppress unchanged replay. Each content transition gets a
stable outbox entry before the provider cursor advances. A busy agent leaves that
entry pending; admission retries reuse the stored content and receipt. Disabling
the source prevents further admission. Daily rebuilds continue CRM reconciliation
without repeatedly notifying about unchanged event contents. Previously unseen
events discovered during a full rebuild or expired-token recovery are baselined
quietly; this does not claim delivery of every historical change. Pending delivery
retries do not refetch a clean calendar before its next scheduled check.

## Deployment

Apply managed D1 migrations through `0008_calendar_notifications.sql` and the Wrangler
`CalendarPushDelivery` Durable Object migration/binding, then deploy managed and
the account callback router. No global cron trigger or Pub/Sub topic is required
for Calendar. The public callback needs valid HTTPS, Calendar API enabled on the
connector's Google project, and the exact connection must grant
`calendar.readonly` or `calendar.events.readonly` (or a compatible broader scope).
Existing OAuth credentials remain in the connector broker.

Local tests use synthetic provider responses, real workerd Durable Objects, and
migrated D1. They do not prove live Google callback delivery. Enable a source only
after deployment and inspect status before claiming a live watch/import is ready.

Protocol references: [push notifications](https://developers.google.com/workspace/calendar/api/guides/push),
[incremental synchronization](https://developers.google.com/workspace/calendar/api/guides/sync),
[events.list parameters](https://developers.google.com/workspace/calendar/api/v3/reference/events/list).
