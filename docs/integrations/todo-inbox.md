# Decision-first mobile TODO

The iPhone app opens the decision list for signed-in accounts. A single-row “On your mind…” composer and a full-width, icon-led TODO/Chat bar share a pinned bottom dock; Chat’s composer stacks just above the switch instead of overlapping it. The bar uses Muse’s floating outlined-icon navigation grammar, soft selected capsule and accessible labels, not an exact five-tab replica. Its remaining width holds the selected Chat conversation’s model, thinking effort and Auto controls, all one-tap accessible even from TODO; these settings never route TODO monitoring. Optional TODO watch instructions expand only when tapped. Both modes use the same native growing text editor, shell, sizing, and send-button geometry; their controls differ only where the operation does (Chat model/attachments/voice vs. TODO watch trigger/save). The TODO list begins directly with actionable cards—no title, refresh button, section header or filter row. Pull down to refresh. Decision rows offer the first choice on a leading swipe and the second on a trailing swipe (tap the revealed button; a full swipe does not auto-approve). Tapping a card opens context, all choices, and free-text editing. TODO lists account-owned captures and decision proposals, keeps a local unsent draft while switching tabs, and refreshes while visible in the foreground. The app never synthesizes production decision cards; its sample email decision exists only behind the debug `--todo-ui-fixture` launch argument.

## Account API

`GET /v1/todo` returns `{items, decisions}` (up to 200 recent captures, 200 open decisions, and 200 recent non-open decisions; older records need pagination). `POST /v1/todo` accepts `{body, watch_hint, operation_id}` with a stable UUID; identical retries return the same saved item. A conflicting use of the same operation ID returns 409. A saved item starts as `captured`, not `watching`: no firehose consumer subscribes to it yet.

Trusted Worker code can call the account Durable Object `proposeTodoDecision({source_key,todo_id?,workflow_id?,title,context,source_label,source_url,choices})`. `source_key` deduplicates a producer's proposal within that account; `todo_id` links an existing capture, while `workflow_id` is a producer-owned correlation key. There is **no public decision-creation endpoint**. A decision may be `needs_you`, `answered`, `resolved`, or `stale`. `POST /v1/todo/decisions/{id}/respond` accepts `{version, choice_id, text, operation_id}` (exactly one choice or text) and records one versioned response idempotently. A successful response changes the decision to `answered`, not `resolved`. A changed version returns 409. The endpoint does not send email, create invites, or resume a workflow; a future producer/consumer must revalidate authority and take responsibility for side effects before marking `resolved`.

The API is authenticated to the user account. Connect grants and service callers are denied; GET requires `agents:read`, writes require `agents:write`, browser mutations require same origin. User data resides in the account's Durable Object SQLite, separate from conversation unread cursors. Capture responses are never interpreted as standing authorization. The client limits and retains its unsent text when the write fails.

## Remaining integration

Only the experimental owner-gated Gmail → bounded Jev classifier → TODO decision producer described below is present. A workflow response consumer, push notification/deep link, capture monitoring status transitions, pagination, and server-side edit/archive controls are **not** present. Do not conflate the one-shot Jev model router with this triage policy or represent it as a universal firehose. Any workflow consumer needs durable cursors, dedupe, exact action/version approval, stale-data checks, and explicit external-write outcomes.

Reference: [Muse from Meta’s official App Store preview](https://apps.apple.com/us/app/muse-from-meta/id6760173601).

## iPhone fixture preview

These images and the recording use `--demo --todo-ui-fixture`; names and email context are fictional. They do not show a live firehose, an executed email action, or a deployed phone build.

| Decisions | Review / edit | Captured | Chat dock |
| --- | --- | --- | --- |
| ![Decision list](media/todo-mobile/decisions.png) | ![Decision detail](media/todo-mobile/decision-detail.png) | ![Captured thought](media/todo-mobile/captured.png) | ![Chat dock](media/todo-mobile/chat-dock.png) |

![Animated fixture walkthrough](media/todo-mobile/walkthrough.gif)

[Watch the full-quality interaction recording](media/todo-mobile/walkthrough.mp4) · [Leading swipe](media/todo-mobile/swipe-primary.png) · [Trailing swipe](media/todo-mobile/swipe-secondary.png)

## Experimental Gmail firehose decision producer

The Managed Worker enables a first, **best-effort** Gmail→Jev→TODO path only for the configured administrator account (`NANOCODEX_FIREHOSE_DECISIONS_ADMIN_ENABLED=true`); an exact `NANOCODEX_FIREHOSE_DECISIONS_OWNER_ID` override is also supported. It runs only when an existing Gmail push watch delivers hydrated INBOX message snapshots. The classifier is separate from the task/model router: it asks whether the sender explicitly requests a personal email reply, and only a validated reply choice with confidence ≥0.85 proposes a card. Missing/truncated body, resync, low-confidence, invalid or unavailable classifications create no card; the normal Gmail agent wake is not held hostage by classifier failure. Durable per-message positive/negative receipts avoid reclassification on replay; invalid or unavailable results remain retryable if the broker replays. The account TODO store deduplicates cards by policy version, connection and Gmail message ID; a failed account write retries with the broker event. Neither Jev confidence nor a proposed card proves a reply is actually needed.

The card contains only a bounded subject and sender, a generic Gmail link, and `Follow up` / `Dismiss` choices. Those choices record intent **only**; they do not draft, send, archive, or otherwise mutate mail. Incoming email is untrusted data, including embedded instructions and links. This first slice does not yet monitor user-captured thoughts, ingest all firehose sources, supply a consumer for choices, or guarantee every message creates a decision. It should not be presented as a full autonomous decision loop.

### Decision traces and labeled backtests

With the owner gate enabled, the private account API `GET /v1/todo/traces?limit=50` returns latest decision metadata; pass its `next_cursor` as `before` for older pages. It includes an opaque source fingerprint, policy version, eligibility/classifier reason, Jev confidence and reply-choice probability when valid, classifier result, timing, whether a decision was persisted, and its decision ID. Both positive and negative/abstaining classifications are represented. No sender, subject, body, prompt, raw model output, provider errors, or credentials are persisted in this trace. It is owner-scoped, capped to 5,000 records and 90 days. Trace persistence failures are logged as a fixed operational event, and never prevent normal Gmail wake; a missing trace can therefore occur during an outage.

`POST /v1/todo/decision-backtest` accepts up to five **caller-supplied, labeled** fixtures per invocation, not a mailbox query. Example JSON:

```json
{"samples":[{"id":"fixture-1","expected":"reply","from":"example@example.test","subject":"Scheduling","body":"Could you reply with a time?"},{"id":"fixture-2","expected":"no_reply","from":"news@example.test","subject":"Newsletter","body":"This week's updates."}]}
```

The owner-only endpoint runs the same Jev choice question in memory through the configured Cloudflare AI binding, returns per-fixture bounded signals and a confusion matrix at thresholds 0.65, 0.75, 0.85 (live), 0.90 and 0.95. It neither stores nor echoes fixtures and never proposes a TODO or sends email. Use de-identified consented fixtures and hold out senders/threads/times; recorded `Follow up`/`Dismiss` choices are not accuracy labels. Jev confidence is a classifier signal, not measured correctness. Tests use synthetic cases; no live historical-mail backtest is claimed.

The firehose route uses AI Gateway ID `default` unless `NANOCODEX_JEV_GATEWAY_ID` names another gateway. Cloudflare's AI binding uses a stored BYOK key only under the `default` alias; a different alias is not selected by the binding. The route disables Gateway request/response log collection for private email content and uses the owner-scoped metadata trace instead. Verify the selected gateway's billing/credential setup and a synthetic backtest before enabling ingestion.
