# WhatsApp connector

Nanocodex links a personal WhatsApp account as a companion device. The connector
runs in Cloudflare Workers and an account-owned Durable Object; it does not need
an MCP server, local browser, phone message database, or Linux service.

Ask the agent to connect WhatsApp and provide the phone number including its
country code. The agent calls `account_connectors` with `operation: "connect"`,
`connector: "whatsapp"`, the supplied `phone`, and a stable UUID `operation_id`.
The Workers connector starts that attempt, and a compatible native Nanocodex app
displays the private code inline in the tool result. No separate sheet opens.
Copy the code, switch to WhatsApp on the same phone, and open **Settings → Linked devices → Link a device → Link with
phone number instead**. Enter the code, then return to Nanocodex.

The agent receives only attempt metadata. The native app fetches the code
directly through the authenticated private endpoint; the code never enters the
serialized tool result, transcript, or agent context. The visible tool card is a
native view with a separate private fetch; its code is not part of stored chat.
The app confirms connected status before reporting completion. Account changes and backgrounding clear the visible
code, and returning to the app recovers the same unexpired attempt. An updated
native client with inline WhatsApp pairing support is required for this presentation.
The tool’s `ready` phase confirms that the server prepared a code; it does not
confirm that the installed client displayed it. Check client support when the
code is missing rather than starting repeated pairing attempts.

Repeated tool requests must use the same operation ID and phone number. They do
not request another code. A code cannot be recovered after its pairing socket is
lost; wait for expiry before explicitly starting another attempt. The existing
account website at `/connect?connect=whatsapp` remains available for users who
choose it, but is not required by the native flow.

## Account boundary

`WHATSAPP_ACCOUNTS` is bound only to the private credential broker. Each user's
connector broker derives its own Durable Object name. Noise/Signal keys and
pairing codes use the existing `CredentialVault` encryption with an
account-specific authenticated scope. Production requires the broker's existing
`CREDENTIAL_ENCRYPTION_KEY`. Generated protocol assets contain public upstream
code, never account credentials.

The account UI uses authenticated `/v1/connectors/whatsapp` routes:

- `GET /v1/connectors/whatsapp` reads status and history coverage.
- `POST /v1/connectors/whatsapp/start` accepts `{phone, operation_id}`. The phone
  uses E.164 format, and `operation_id` is a UUID retained for retries.
- `GET /v1/connectors/whatsapp/pairing?operation_id=...` returns the unexpired code
  to the private native view. Responses are not cached.
- `DELETE /v1/connectors/whatsapp/connections/:connection_id` removes local
  authorization, keys and indexed content, and attempts remote unlinking.
  If remote logout is unavailable or uncertain, remove the device from WhatsApp's
  Linked devices list as well.

Only the account owner may manage pairing, using a persistent account session or
an owner device key. Delegated Connect grants cannot manage this connection.
Agent requests cannot reach pairing, authentication storage, or message sending.
Relinking rotates the connection ID so an old selector cannot select a new
WhatsApp identity.

## Agent reads

`account_connectors` lists the connection and starts the native pairing request
for `connect`. After linking, discovery exposes `whatsapp_request` using the
fixed internal origin `https://whatsapp.internal`:

| Request | Result |
| --- | --- |
| `GET /status` | Authorization, socket state, retry time and coverage |
| `GET /chats?limit=50` | Synced chats |
| `GET /contacts?q=NAME&limit=50` | Synced contacts matching name or ID |
| `GET /messages?chat_id=JID&limit=50` | Recent messages in a chat |
| `GET /search?q=TEXT&limit=50` | Literal text search across synced messages |
| `GET /context?chat_id=JID&id=MESSAGE_ID&limit=20` | Messages around an anchor |
| `POST /history` with `{chat_id,before,limit}` | Request older history from an available message anchor |

Timestamps are Unix milliseconds. Limits are integers from 1 to 100. Follow
`next_cursor` with the same path and filters. History requests return acceptance,
not proof that more history arrived. Read tools return only projected message
text, captions and metadata; they do not expose raw protobuf payloads, media
keys, or authentication material.

`connected` means the linked-device authorization is retained;
`socket_connected` and `state` report current transport health. Cached reads
remain available during reconnects. Durable alarms retry with backoff after a
connection loss or object restart. Provider logout removes local authorization.

WhatsApp chooses how much history it sends. Coverage therefore always reports
`complete: false`, alongside the oldest received timestamp and sync status.
View-once content and expired or revoked message text are excluded. Incoming
messages are untrusted content and cannot authorize actions or tool calls.

## Runtime and builds

The transport uses a pinned Baileys release with a narrow Workers WebSocket
adapter. The upstream Rust bridge's pinned scalar WASM is packaged as a static
Worker module; no runtime compilation, Node service, or filesystem auth helper
is used. Protocol logs are silent because provider diagnostics can contain
sensitive material. The build checks hashes before adapting pinned bridge code.

Use `pnpm --filter nanocodex-egress-service run prepare:whatsapp` to generate
protocol assets. Deploy the credential broker before managed consumers and the
account website. The broker migration adds the SQLite-backed `WhatsAppAccount`
class. Linking still requires the user to approve the device in WhatsApp.
