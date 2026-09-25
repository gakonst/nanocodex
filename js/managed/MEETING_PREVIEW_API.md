# Meeting preview (opt-in)

This endpoint accepts **finalized text segments only** and produces a bounded rolling recap while recording. It does not create an agent or submit a managed turn. The iOS device remains authoritative for the complete transcript; the server retains only a compact summary plus text not yet summarized. No audio, partial Speech result, or final agent-thread identity is accepted.

Deployment must set `NANOCODEX_MEETING_PREVIEW_ENABLED=true` and bind the `MeetingPreview` Durable Object. A disabled deployment returns HTTP 503. Roll out the managed route and account proxy before installing an iOS build that relies on generated previews; preview failures never gate the final agent turn.

## Contract

Use an authenticated, persistent full account session or full account API key with `agents:read`, `agents:write`, and `tools:use`. Connect grants and `nci_live_` inference keys cannot use this endpoint. Browser cookie mutations require `Origin` equal to the request's origin. The path UUID is the local capture ID (case insensitive), unrelated to any eventual agent thread. Do not send credentials, owner IDs, raw audio, or previous summaries in JSON.

`POST /v1/meetings/{captureUUID}/preview` JSON, exactly:

```json
{"revision":1,"delta":"Finalized transcript segment text. "}
```

- Send segments in spoken/capture order, one revision per segment, starting with 1 and incrementing by exactly one. Do not submit transient Speech partials. The delta is appended once; the server separates it from the next delta with a newline. Do not resend the entire transcript.
- Success HTTP 200: `{ "capture_id":"<lowercase UUID>", "revision":1, "summary":"...", "summary_revision":1, "status":"updated" }` on a completed new recap. `status:"pending"` means the segment was committed but a recap has not run (below text threshold, cooldown or budget); `summary_revision` can lag `revision`, and `summary` may initially be `""`. `status:"unavailable"` means generation failed but the segment was durably retained for the next eligible attempt. The same revision and exact delta replay returns `status:"unchanged"` and the current snapshot without another generation. The response is not a guarantee that the recipient read or displayed the recap.
- `GET` on the same path recovers the current `{capture_id,revision,summary,summary_revision,status:"unchanged"}` snapshot; it does not expose raw transcript. `DELETE` returns 204, clears text and recap, and tombstones the capture. Do this after final handoff; abandoned captures expire automatically. A late POST to a deleted capture returns 410.
- A different delta for an already committed revision returns 409 `revision_conflict`. A gap returns 409 `{error:"revision_gap",expected_revision:N}`. Concurrent account requests return 409 `meeting_busy` with `Retry-After: 1`; retry the **identical** revision and delta (or GET first). A wrong owner or organization/team authorization epoch sees 404.

The server accepts at most 8 KiB JSON bodies, 4 KiB UTF-8 per delta, 1,024 revisions/capture and 24 KiB not-yet-summarized text (including the newline inserted after every delta). Excess pending context is 413; the client should keep its full local transcript and avoid discarding unacknowledged segments. Inactivity expires the capture after 3 hours and removes its text via a Durable Object alarm; GET then returns 404, POST returns 410 while its tombstone is retained. Tombstones are purged after a further 24 hours, so a caller must never reuse a capture UUID. At most 8 captures may be active concurrently and 60 new captures are admitted per user per UTC day; excess starts return 429. Request/response caching is disabled.

Generation starts when at least 80 bytes of finalized text are pending, and no sooner than 45 seconds after the previous attempt on that capture. The explicit Workers AI GLM-5.3 low-effort candidate avoids an additional routing-classifier inference. The direct Responses adapter caps output at 320 tokens and the retained recap at 2 KiB. Per-account ceilings across captures are 2 generation attempts per fixed UTC minute and 120 per fixed UTC day; a budget-limited POST still durably accepts its delta and returns `pending` with `retry_after_seconds`. Only a subsequent new revision can trigger a pending recap. One account's Durable Object serializes generation and admissions; a slow provider can return `meeting_busy` to additional writes until completion or timeout (120 s).

Previous compact recap and unprocessed segments are sent to the provider for each eligible attempt; no full-history recovery, quality guarantee, provider-retention guarantee, or background summary push is promised. Provider failure consumes an attempt, retains pending text and returns a stale summary. A successful HTTP response may still contain `pending`/`unavailable`; display `summary_revision` as the actual coverage, not the latest transcript position. Keep full text and final-review/send logic on the device. This endpoint never posts to or creates a managed agent thread.
