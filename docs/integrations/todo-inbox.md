# Mobile TODO inbox

The bottom-left tab combines upcoming Calendar events, firehose decisions, captured tasks, and Gmail conversations in one compact queue. The full-width app selector and “On your mind…” capture composer remain in the bottom dock.

**For you** prioritizes events starting within an hour, decisions, drafts, captured tasks, later events, and recent mail. **Mail** offers Inbox, Unread, Drafts, Sent, and All mail, with an account selector and Gmail search syntax. **Later** shows locally snoozed rows. Rows have stable account/source identities: email decisions use exact connection and thread references, never subject matching. Legacy decisions without those references keep their original context sheet.

Opening mail shows the conversation’s messages, recipients, dates, body text, and downloadable attachments. Messages expand independently or together. HTML-only mail is projected to inert text; remote content and scripts do not run. Previous/next controls move between the loaded conversations. Compose, reply, reply all, and forward open an editable recipient/subject/body editor. Forward currently includes message text; original attachments remain accessible in the reader.

Draft edits are recovered from protected, account-scoped device storage and saved to the account server. “Draft for me” explicitly requests a proposed reply, which remains editable. It has no mail-sending tools. Only tapping **Send** submits the saved draft’s exact version. The editor freezes during this submission. A durable receipt prevents another provider send for the same draft, including after a timeout or a new operation ID. Unknown delivery stays locked and exposes a read-only status check. A version conflict preserves local edits for review rather than silently overwriting either version.

Trailing swipes offer snooze and mail archive; archive and captured-task completion have Undo. Snooze is device-local and returns an item when the app next refreshes after its due time; it does not modify Gmail or create a push reminder. Snoozed mail summaries remain available after they leave the first inbox page. Captured task completion is versioned and durable on the account server. A confirmed reply resolves only the firehose decision for that exact original message.

The queue refreshes on foreground entry, pull-to-refresh, after closing a detail, and every minute while visible. Search is debounced. Calendar and mail errors remain visible alongside usable sources. This is a bounded view: 25 conversations per account/page, explicit Load more, up to 100 saved review drafts, and the next 14 days of Calendar events with a partial-coverage indicator. Review drafts are stored by Nanocodex; they are separate from Gmail’s native Drafts folder. Large provider responses can fail the bounded reader rather than being silently presented as complete.

## Account API

All routes below are under `/v1/todo`, authenticated to the account owner’s data, and unavailable to Connect grants or service principals. Reads require `agents:read`, mutations require `agents:write`; browser mutations require same origin. Provider credentials remain in managed connector egress. The client sends exact opaque connection IDs when selecting an account.

| Route | Contract |
| --- | --- |
| `GET /` (without a trailing slash) | Existing `{items, decisions, traces, feed_bounds}` decision/capture projection. |
| `POST /` (without a trailing slash) | `{body, watch_hint, operation_id}` saves a thought. Saving does not authorize external actions. |
| `PATCH /items/{id}` | `{version, status: "done" | "captured", operation_id}` completes or restores a capture. Retries return the original receipt without reapplying it. |
| `POST /decisions/{id}/respond` | Existing `{version, choice_id, text, operation_id}` records one choice/instruction. It does not send email. |
| `GET /mail/accounts` | `{accounts:[{connection_id,label,email,capabilities,scopes}]}`. |
| `GET /mail/threads?connection_id=&q=&page_token=` | `{threads:[{id,connection_id,subject,snippet,from,date,unread,message_count}],next_page_token}`. |
| `GET /mail/threads/{id}?connection_id=` | `{thread:{id,connection_id,subject,messages}}`, including readable bodies, reply headers, completeness flag, and attachment metadata. |
| `GET /mail/threads/{id}?connection_id=&format=metadata` | Lightweight `{summary}` including `in_inbox` for reminder reconciliation. |
| `GET /mail/messages/{id}/attachments/{attachment}?connection_id=` | `{data,size}`, using base64url data. |
| `GET /mail/drafts?connection_id=&thread_id=` | `{drafts}`. The thread filter is optional. |
| `GET /mail/drafts/{id}` | `{draft}`, including delivery status. |
| `POST /mail/drafts` | Saves a review draft with `id`, exact `version` (0 for initial create), `connection_id`, `mode`, `to`, `cc`, `bcc`, `subject`, `body_text`, and optional `thread_id`/`reply_message_id`. Recipient values are email-address arrays. |
| `POST /mail/suggest` | `{connection_id,thread_id,reply_message_id,instructions?}` returns `{body_text}` for review. It neither saves nor sends. |
| `POST /mail/send` | `{draft_id,version,operation_id}` returns `{receipt}` with `sent` or `unknown`. No automatic resend after uncertainty. |
| `POST /mail/threads/{id}/modify` | `{connection_id,archive?:Bool,unread?:Bool}`; `archive:false` restores the Inbox label for Undo. |
| `GET /schedule?connection_id=&from=&to=` | `{events,partial,errors,from,to}` across available calendars. Connection and interval filters are optional. Dates are RFC3339 or `YYYY-MM-DD` for all-day events. |

Decision source fields are additive and nullable: `source_connection_id`, `source_thread_id`, and `source_message_id`. The internal Gmail producer supplies validated references. Existing SQLite records remain readable after migration. Other workflow decision responses retain their existing `answered` state until their own consumer processes them.

Mail drafts support `compose`, `reply`, `reply_all`, and `forward`. `reply_message_id` is the provider message ID; the server retrieves the original RFC Message-ID and References for threading. Send reconstructs MIME from the reviewed fields, validates headers, and obtains the sender address from the selected account. External failures return bounded error codes without provider credentials or raw response bodies.

## Firehose and diagnostics

The owner-gated Gmail → Jev classifier remains the decision producer (`NANOCODEX_FIREHOSE_DECISIONS_ADMIN_ENABLED=true`, or an exact `NANOCODEX_FIREHOSE_DECISIONS_OWNER_ID`). It consumes hydrated INBOX push snapshots, classifies whether a personal reply was explicitly requested, and proposes a decision only for a validated reply with confidence at least 0.85. Durable per-message receipts deduplicate replays. Missing/truncated input and failed/low-confidence classifications do not create an actionable card. The UI keeps recent diagnostic activity below the queue.

`GET /v1/todo/traces?limit=50&before=` pages private trace metadata. It records policy version, outcome/reason, confidence, timing, decision ID, and bounded sender/subject metadata. Bodies, prompts, provider errors, and credentials are not stored. Records are account-scoped, capped at 5,000 and 90 days; the main feed includes at most 100 recent non-duplicated diagnostics.

`POST /v1/todo/decision-backtest` accepts up to five caller-supplied labeled fixtures (`id`, `expected: "reply" | "no_reply"`, `from`, `subject`, `body`). It returns classification signals and confusion matrices at the documented thresholds, without fetching historical mail, saving fixtures, proposing decisions, or sending messages. Jev confidence is not measured correctness.

Draft suggestions use the existing Workers AI binding with a bounded prompt and response, no tools, and AI Gateway request/response collection and caching disabled. They use at most the latest six messages through the selected reply target. They can be unavailable independently of reading or manually composing mail. Captured-thought monitoring, general workflow execution, notification delivery, outgoing file attachments, rich-text editing, Gmail-native draft synchronization, and cross-device snooze are separate features.

## Validation and evidence

Run the authenticated HTTP journey with `pnpm --filter nanocodex-managed-service run test:todo-mail`. It exercises the real Worker/Durable Object transport with synthetic Google/AI dependencies: multi-account reads, full message/attachment retrieval, draft save/reopen/edit, exact-version send, duplicate and ambiguous send handling, authorization, and schedule coverage.

Eight TODO journeys in `InboxUITests` cover the unified queue, full thread, draft suggestions/edit/reopen/explicit send, uncertain-send relaunch, task/archive Undo, sender-aware compose-again, snooze/restore across linked rows, and distinct message decisions within one thread. They use `--demo --todo-ui-fixture --todo-mail-fixture` with synthetic mail and never send a real message. Use `scripts/xcodebuild-guard.sh` on a shared Mac. Keep screenshots, recordings, and result bundles in ignored `output/` or CI artifacts, not tracked source. A simulator fixture run does not demonstrate a production deployment.
