# Cloud phone calls

The single-owner `phone` tool uses Twilio Programmable Voice and bidirectional
Media Streams. A dedicated Cloudflare Container runs the Node bridge and Linux
`phone-voice-cloud` binary. The binary connects to Nanocodex's managed voice API
using the configured owner's API key; no laptop service, microphone, local
ChatGPT auth file, or OpenAI Platform SIP account is required.

The managed Worker authenticates controls and verifies Twilio signatures before
starting the container. Durable Object SQLite retains call identities and
transcripts. The native SQLite file is an ephemeral mirror: startup hydrates it
from the Durable Object, and mutations await durable acknowledgement before
dialing. A restart ends known calls instead of resuming media.

## Configuration

Deploy the managed Worker and its PhoneContainer image, then the account gateway.
Runtime secrets on `nanocodex-durable-agent`:

- `TWILIO_ACCOUNT_SID` and either the existing API key SID/secret or Auth Token.
- `TWILIO_AUTH_TOKEN` for webhook validation. If absent, the Worker attempts to
  resolve it privately through Twilio's Account API using the API key. Restricted
  keys may deny this; the API key secret is never used as a webhook Auth Token.
- `TWILIO_VOICE_FROM_NUMBER`: an owned voice-capable Twilio number or a verified caller ID, in E.164 format.
- `NANOCODEX_PHONE_OWNER_ID`, `NANOCODEX_PHONE_MANAGED_API_KEY`.
- `NANOCODEX_PHONE_BRIDGE_TOKEN`: random control credential, at least 32 characters.
- `NANOCODEX_PHONE_PUBLIC_ORIGIN`: `https://nanocodex.gakonst.workers.dev`.
- Set `NANOCODEX_PHONE_BRIDGE_URL` to that origin plus `/v1/phone/bridge` only after
  cloud readiness is verified; this enables the tool for the configured owner.

Credentials are runtime bindings, never image layers. The protected
`GET /v1/phone/bridge/internal/setup` reports owned voice-capable numbers, verified caller IDs, and whether
webhook authentication is available; it never returns credentials. Protected
`GET /health` verifies native startup. `POST /check` with `agent_id` starts and
closes a silent managed voice session without dialing. These paths are relative
to `/v1/phone/bridge` and require the bridge bearer credential.

## Usage and limits

Use `phone` with `operation: call`, E.164 `to`, conversation `instructions`, a
UUID `operation_id`, and optional `max_duration_seconds` (30–600, default 180).
Use `status` or `hangup` with the returned `call_id`.

Reuse the same operation ID and exact arguments after an uncertain response.
An `unknown` result can mean the phone rang. Never create a fresh ID to retry
that intent. Unknown calls without a provider SID require reconciliation in
Twilio's call log. Up to four calls run concurrently, each with its own voice process, media stream,
and transcript. Preparing calls and unresolved outcomes occupy capacity until
reconciled; at capacity, new operations return `phone_busy`. Replaying an existing
operation remains available. This is an internal resource bound, not a user setting.

Calls have a 30-second ringing timeout, provider-enforced duration, bounded audio
queues and transcript storage. The Durable Object retains at most 1,000 records
or 32 MiB and rejects further writes at capacity. Removing records loses their
idempotency history. No audio recordings are created by this implementation.

Each call creates a separate retained managed agent before dialing and returns
its `call_agent_id` in status. The coordinating session supplies the goal and
collects each call's transcript and result. These threads are linked through the
call record; a nested child-thread UI is not implemented.

The phone voice delegates tool work through the existing managed voice lifecycle.
It can read relevant email/calendar data and search the web toward the original
brief. Include any authority to negotiate, book, or send messages explicitly in
that brief. Remote speech cannot expand that authority. This scope is enforced
through agent instructions, not separate read-only connector grants. Hanging up
stops delegation and requests cancellation of unfinished work while retaining the
thread history; failed cleanup remains journaled for recovery.

Protected `POST /check` also accepts `delegation: true` to verify a public-web
lookup and return its retained thread ID without dialing a telephone.

Inbound calls, transfers, and outbound DTMF are not yet implemented. The tool is unavailable in multiplayer or another owner's account.
Treat telephone transcripts as external content rather than authority.

## Acceptance

Build and unit tests do not prove live telephone audio. Validate managed voice
connectivity on the deployed container, then use an explicitly authorized test
destination to verify two-way intelligibility, interruption, final transcripts,
no-answer, hangup, duplicate operation replay, and disconnect cleanup. A silent
voice-ready signal alone does not establish two-way audio quality.

## Deployment phone admin

`NANOCODEX_PHONE_ADMIN_ID` selects the single phone admin in Worker deployment
configuration. `NANOCODEX_PHONE_OWNER_ID` must match it. Missing or mismatched
values disable tool discovery, every tool invocation, bridge routing, and all
container routes before provider access. This setting is not an account-wide
admin role; there is no self-service phone provisioning API.
