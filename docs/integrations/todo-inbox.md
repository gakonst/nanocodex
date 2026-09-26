# Decision-first mobile TODO

The iPhone app opens the TODO surface for signed-in accounts. Chat remains an adjacent tab. TODO lists account-owned captures and decision proposals, keeps a local unsent draft while switching tabs, and refreshes while visible in the foreground. The app never synthesizes production decision cards; its sample email decision exists only behind the debug `--todo-ui-fixture` launch argument.

## Account API

`GET /v1/todo` returns `{items, decisions}` (up to 200 recent captures, 200 open decisions, and 200 recent answered decisions; older records need pagination). `POST /v1/todo` accepts `{body, watch_hint, operation_id}` with a stable UUID; identical retries return the same saved item. A conflicting use of the same operation ID returns 409. A saved item starts as `captured`, not `watching`: no firehose consumer subscribes to it yet.

Trusted Worker code can call the account Durable Object `proposeTodoDecision({source_key,todo_id?,workflow_id?,title,context,source_label,source_url,choices})`. `source_key` deduplicates a producer's proposal within that account; `todo_id` links an existing capture, while `workflow_id` is a producer-owned correlation key. There is **no public decision-creation endpoint**. A decision may be `needs_you`, `answered`, `resolved`, or `stale`. `POST /v1/todo/decisions/{id}/respond` accepts `{version, choice_id, text, operation_id}` (exactly one choice or text) and records one versioned response idempotently. A successful response changes the decision to `answered`, not `resolved`. A changed version returns 409. The endpoint does not send email, create invites, or resume a workflow; a future producer/consumer must revalidate authority and take responsibility for side effects before marking `resolved`.

The API is authenticated to the user account. Connect grants and service callers are denied; GET requires `agents:read`, writes require `agents:write`, browser mutations require same origin. User data resides in the account's Durable Object SQLite, separate from conversation unread cursors. Capture responses are never interpreted as standing authorization. The client limits and retains its unsent text when the write fails.

## Remaining integration

The firehose → bounded Jev classifier → decision producer, workflow response consumer, push notification/deep link, capture monitoring status transitions, pagination, and server-side edit/archive controls are **not** present in this slice. Do not represent the current Gmail push wake or one-shot Jev model router as this pipeline. Any workflow consumer needs durable cursors, dedupe, exact action/version approval, stale-data checks, and explicit external-write outcomes.
