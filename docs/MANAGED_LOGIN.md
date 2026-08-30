# Managed login and hosted connections

This document is the canonical design for the first-party interactive
`nanocodex login` and `nanocodex connect` flows. Agents changing CLI login,
Nanocodex Connect, Accounts device authorization, hosted-agent grants, or
connection onboarding must read it before editing those surfaces.

## Product contract

The commands are:

```text
nanocodex login
nanocodex connect <chatgpt|github|gmail|gdrive|x|whoop>...
```

"Login" means creating or signing into a Nanocodex account and authorizing this
Nanocodex installation. It does not mean logging directly into ChatGPT.
It requests the base hosted-agent, history, and memory capabilities only.

"Connect" means connecting one or more named hosted services and attaching them
to this installation. It may reuse an existing browser account session, but it
still uses an explicit installation authorization: selecting an account in the
browser does not itself grant the CLI access. Each request grants exactly the connectors
named on that command line; it never inherits hidden connector requirements from
an earlier account or installation grant.

The Account page is the persistent catalog for connecting, disconnecting, and
inspecting services independently of a CLI request. A CLI connect page is the
focused form of the same UI and shows only the connector named by the signed
request.

The interactive CLI receives a scoped Nanocodex Connect grant. It must not
silently mint a general-purpose Nanocodex API key. API keys remain an explicit
operation for CI, curl, or another machine.

The flow uses Tempo Accounts/Wata `wallet_connect`. This name refers to the
Tempo Accounts RPC method, not the WalletConnect protocol.

## Local development

`cd web && npm run dev` owns the complete local login topology. The website,
managed account service, credential broker, and Connect API run in one
Cloudflare Vite multi-Worker session with retained local Durable Object state.
Use the exact loopback CLI command printed by the launcher; for the primary
checkout it is:

```text
NANOCODEX_CONNECT_DEVICE_BASE_URL=http://nanocodex.localhost:5173/v1/device nanocodex login
NANOCODEX_CONNECT_DEVICE_BASE_URL=http://nanocodex.localhost:5173/v1/device nanocodex connect github
```

Pass `--no-open` to either command in a headless shell. The CLI still prints the
complete verification URL and confirmation code for the user to open elsewhere.

The reserved `.localhost` name keeps the native transport loopback-only and
gives every worktree the stable `nanocodex.localhost` WebAuthn RP ID. No
OrbStack, Docker, local TLS, or `.local` DNS is required. OAuth providers return
through the stateless relay on `127.0.0.1:47891`, which verifies a short-lived
signed routing envelope before returning to the initiating worktree and its
private token exchange. Provider dashboards use the four exact callback URLs
documented in `docs/LOCALHOST_MIGRATION_PROMPT.md`; never register a worktree
origin or wildcard callback.

## End-to-end flow

1. **Choose one action.** `nanocodex login` requests the base installation
   grant. `nanocodex connect <service>...` requests exactly the named connectors.
   One service is focused and starts automatically; multiple services stay
   explicit so the user chooses each connection action.

2. **Check the existing login.** Login loads its local Nanocodex login record.
   If its base grant is valid, it reports the account, hosted agent, expiry, and
   capabilities instead of starting a redundant ceremony. Connect deliberately
   starts a new ceremony because it changes the installation grant.

3. **Prepare the requested authorization.** The CLI prepares a Tempo access key
   so the existing signed result remains interoperable and MPP can use its
   required spending policy. Private signing material remains on the machine.
   A non-MPP request explicitly includes
   `urn:nanocodex:authorization:hosted`, allowing Connect to approve hosted
   authority without returning or persisting that key. MPP requests omit this
   marker and require the signed access-key result.

4. **Register a device authorization.** The CLI starts a Wata device-code
   exchange containing one `wallet_connect` RPC request. The request declares
   the exact capabilities this installation wants:

   - run a hosted Nanocodex agent;
   - read hosted conversation history;
   - use hosted durable memory;
   - for `connect`, grant exactly the connector services selected in the command;
   - optionally use MPP under an explicit access-key spending policy.

   A connect request also signs a singular connector-focus resource. The focus
   must be part of the exact signed connector set.

5. **Open Nanocodex Connect.** The device-code host returns a user code and
   verification URL. The CLI prints both and opens the verification URL in the
   browser when possible. `--no-open` suppresses only that automatic launch.

6. **Establish the Nanocodex account.** Nanocodex Connect signs an existing user
   in with their passkey or creates a new passkey-backed Nanocodex account. The
   verified Tempo address is linked to that account.

7. **Run the focused action.** The browser derives its work from the signed
   `wallet_connect` request:

   - login requests installation access and no connector catalog;
   - connect shows only the focused service;
   - an already connected focused service needs no redundant OAuth ceremony;
   - show access-key limits, expiry, scopes, and funding policy when MPP is
     requested;
   - show history and memory access as permissions, without inventing an
     external login step for them.

   The persistent Account page remains available as the complete service
   catalog.

   A focused CLI connection stays in the current browser tab. OAuth services
   navigate that tab to the provider and return through the fixed callback
   relay to a compact connected/not-connected result page. The embedded
   `/connect-dialog` surface may use its own dialog behavior; it is not the CLI
   device flow.

8. **Approve the installation automatically.** Once the passkey-backed account
   is established, the device wizard signs and submits the exact pending
   installation request without a separate account-card or approval click. If
   exactly one saved account is available, the wizard starts that passkey
   ceremony immediately. Account choice, passkey creation, provider OAuth, and
   MPP policy remain interactive when they genuinely require user input. The
   final browser view is the completed installation CTA. The embedded
   `/connect-dialog` approval remains explicit.

9. **Return the explicit Accounts result.** Nanocodex Connect submits the approved
   `wallet_connect` result to the Wata device-code host. The CLI polls with PKCE
   and receives exactly one account. The result is either the existing signed
   access-key authorization or a hosted authorization containing a 43-character
   one-time approval identifier and `mode: "hosted"`. The CLI accepts hosted
   mode only for a non-MPP request carrying the hosted resource marker.
   Provider credentials are never returned to the CLI.

10. **Create the Nanocodex Connect grant.** The CLI exchanges the approval with
    the Connect API using an explicit `authorization_mode`. `access_key` sends
    the existing key authorization fields. `hosted` sends the app, account,
    approval, permission, and requested connectors with no access-key material.
    The platform validates the account link, resources, connector readiness,
    and authorization policy; creates or reuses the user's hosted agent and
    private egress subject; and returns a response with the same mode, a scoped
    grant token, agent identifier, exact capabilities, and bounded expiry.

11. **Persist the login locally.** Owner-only `connect.json` retains the scoped
    grant token, account and agent identifiers, expiry, approved capabilities,
    and an access-key identifier only for access-key mode. Hosted mode writes no
    Tempo key. Rotation and logout retire a local Tempo key only when the stored
    mode has one. Neither mode retains ChatGPT, OpenAI, or connector credentials
    received from the platform.

12. **Report the resulting state.** The command ends with a stable summary, for
    example:

    ```text
    Logged in to Nanocodex

    Account       c884e3c5…480e
    Hosted agent  agent_…
    ChatGPT       connected
    History       enabled
    Memory        enabled
    GitHub        not requested
    Grant expires Sep 25, 2026
    ```

## Capability and storage boundaries

- Tempo Accounts/Wata owns the device-code transport, PKCE exchange,
  `wallet_connect` envelope, and signed access-key authorization. Nanocodex
  Connect owns the one-time hosted authorization when that explicit mode is
  requested.
- Nanocodex Connect owns the application identity, requested Nanocodex
  resources, account linking, connector onboarding, consent UI, and scoped
  grant.
- The private egress broker exclusively owns hosted ChatGPT, OpenAI, and other
  provider credentials. Agent actors and CLI clients retain only opaque
  subjects or scoped grants.
- A local ChatGPT credential is never uploaded implicitly. In local
  development, `nanocodex connect chatgpt` is the explicit user-approved action
  that claims an available local Codex/ChatGPT credential into the broker when
  that account does not already have ChatGPT connected. The credential remains
  broker-owned and is never returned to the browser or CLI grant.
- Durable memory is always hosted and account/team scoped. There is no local
  durable-memory store to merge with it.
- Hosted history is account/team scoped. A consumer may separately search local
  session history, but that is a retrieval concern after login, not part of the
  account or credential ceremony.
- Interactive logout revokes or discards the scoped grant and removes local
  installation secrets. It must not disconnect the user's hosted connectors or
  delete their Nanocodex account unless explicitly requested.
- A connector is an account credential boundary, not automatically an MCP
  server. MCP-native services may attach a concrete MCP transport. ChatGPT,
  GitHub, Gmail, Drive, and X continue to use their native broker/API adapters
  unless the product deliberately assigns an MCP endpoint to one of them.
- A remote OAuth MCP is represented by a broker-generated opaque connection ID.
  `nanocodex connect mcp.linear.app` authorizes that exact ID, while the endpoint,
  OAuth registration, PKCE material, and provider tokens remain broker-private.
  Native agents attach the ID through the grant-scoped Connect proxy and expose
  its tools through the existing deferred `tool_search` path.

## Browser behavior

The browser should compute steps from requested capabilities and current
account state. Examples:

| Entry point | Browser work |
| --- | --- |
| Account page | Show the complete connector and API-key catalog |
| `nanocodex login` | Select/create an account and authorize the installation |
| `nanocodex connect chatgpt` | Show only the ChatGPT connection action |
| `nanocodex connect github` | Show only the GitHub connection action |
| `nanocodex connect chatgpt github ...` | Show every requested connector with no automatic selection |
| `nanocodex connect mcp.linear.app` | Authorize the focused remote MCP in the same tab, then settle when connected |
| `nanocodex connect mcp.linear.app github ...` | Show every requested connector/MCP with no automatic selection |
| Other single `connect` services | Show only the corresponding connector action |
| Any request with MPP | Show the local access-key expiry, scopes, and limits |

Once account identity, connector readiness, and policy review are satisfied,
the browser settles immediately. It must not add a second generic Continue
step.

### Explicit ChatGPT credential import

`nanocodex connect chatgpt` imports the currently configured Codex ChatGPT
login only as part of that explicit approval ceremony. `--auth-file` may
override the source for a request that includes ChatGPT; otherwise the CLI uses
`NANOCODEX_AUTH_FILE`, `$CODEX_HOME/auth.json`, or `~/.codex/auth.json` in that
order. Plain `nanocodex login` and every connect request that omits ChatGPT must
not resolve or open the Codex auth file.

Before device registration, the CLI opens the file once without following its
final symlink where the platform supports that protection. It accepts only a
regular file owned by the current user, inaccessible to group and other Unix
users, and no larger than 64 KiB. The document must be a ChatGPT-mode Codex
login with bounded ID and access JWTs and a bounded refresh token. The ID and
access JWT account and FedRAMP claims must agree with the stored account ID. The
refresh token is an opaque value: it must be nonblank and control-free, but is
not decoded as a JWT. The access expiry must be more than five minutes in the
future.

The signed `wallet_connect` resources contain exactly one import commitment:

```text
urn:nanocodex:credential-import:chatgpt:codex-auth-v1:sha256:<base64url-sha256>
```

The digest input is the bytes
`nanocodex/chatgpt-credential-import/v1\0`, followed in order by the access
token, refresh token, and account ID, each encoded as a big-endian `u32` UTF-8
byte length and the UTF-8 bytes, then the expiry in Unix epoch milliseconds as
a big-endian `u64`, then one FedRAMP byte (`0` or `1`). After the wallet result
has been validated, `/v1/connections` receives `chatgpt_credential_import` with
exactly `access_token`, `refresh_token`, `account_id`, `expires_at`, and
`fedramp`. These credentials are ephemeral request material and are never
written to `connect.json` or returned in CLI diagnostics.

## Implementation slices

Implement this as complete vertical slices rather than extending the old
ChatGPT-specific `auth login` path:

1. Host the Accounts/Wata device-code handler and authenticated approval route
   in Nanocodex Connect, backed by durable Cloudflare storage.
2. Make the existing Connect UI render and settle device-code
   `wallet_connect` requests through the same account, connector, and consent
   components used by browser Connect.
3. Add the Rust device-code consumer and Accounts-store bootstrap needed by the
   native CLI. Keep product-specific grant exchange and storage under
   `bin/nanocodex`.
4. Add top-level `nanocodex login`, `nanocodex connect`, `nanocodex status`, and
   `nanocodex logout` commands over that contract; remove or deliberately
   migrate the old meaning of `nanocodex auth login`.
5. Exercise the complete flow in the real browser, including registration,
   returning login, missing ChatGPT, already-connected ChatGPT, denial,
   expiration, and absence of provider credentials in browser storage and
   network responses.

At the time this design was recorded, the JavaScript Accounts device-code
transport existed, while the native Rust path still needed an equivalent Wata
consumer/bootstrap integration. `tempo-alloy` could consume an Accounts store
but did not itself perform this device-code `wallet_connect` ceremony. Recheck
upstream before implementing rather than preserving that observation as a
compatibility constraint.
