# MPP Responses WebSocket integration

## Boundary

MPP is composed by `bin/nanocodex` and the private, non-published `mpp-egress`
support crate. No public Nanocodex library crate depends on or contains payment
code. The CLI starts an in-process loopback WebSocket adapter and gives its URL
to the normal Nanocodex Responses configuration.
Consequently the existing persistent socket, typed stream processing, retry
policy, retained history, `previous_response_id`, and `store: false` behavior
remain unchanged.

```text
Nanocodex ResponsesService (unchanged)
               |
       ordinary OpenAI frames
               |
      CLI loopback WS adapter
               |
 alloy-transport-mpp application socket
               |
 canonical MPP frames + native TIP-1034 vouchers
               |
          mpp-proxy/mppx
               |
      OpenAI Responses WebSocket
```

The loopback adapter does not forward Nanocodex's OpenAI bearer credential.
It forwards only the Responses beta, cache/session identity, timing, and user
agent headers needed by the upstream proxy.

## Canonical paid socket

The generic application transport belongs in `alloy-transport-mpp`, below its
Alloy JSON-RPC adapter. It carries arbitrary UTF-8 application data and owns the
payment framing:

1. Convert the `ws:`/`wss:` endpoint to `http:`/`https:` and issue an HTTP GET
   probe.
2. Parse every `WWW-Authenticate: Payment` challenge and choose the first one
   supported by the configured `PaymentProvider`.
3. Upgrade the WebSocket and send
   `{"mpp":"authorization","authorization":"Payment …"}`.
4. Wait for `payment-receipt` before exposing the socket.
5. Wrap application data as `{"mpp":"message","data":"…"}` and unwrap the
   same server envelope.
6. Intercept `payment-need-voucher`, obtain the requested cumulative voucher,
   and send it as another in-band authorization.
7. On shutdown, request `payment-close-ready`, sign a descriptor-bearing native
   close credential for the receipt's exact `spent` amount, and wait for the
   final receipt. The client rejects a close amount above its locally signed
   voucher ceiling.
8. Keep receipts and payment errors out of the application stream.

Receipt payloads remain opaque JSON at this layer. That permits any
MPP-compatible provider or future receipt extension; only the CLI's Tempo
session wrapper understands native voucher requests.

The old `alloy-transport-mpp` implementation waits for a noncanonical
in-socket `challenge` frame and uses `type`/`credential` envelopes. Current MPP
uses the HTTP 402 probe and `mpp`/`authorization` envelopes. Nanocodex must use
the canonical application socket, not that legacy dialect.

## Native Tempo sessions

The CLI reads the active account and extractable P-256 access key written by
Tempo Wallet login at `~/.tempo/wallet/store.json`. It uses Alloy's generic
signer interface with a Tempo keychain-v2 envelope; there is no Nanocodex
signer abstraction and no raw private-key CLI option. The same access key can
therefore open and voucher sessions from Wallet CLI, MPPx, or Rust.

`mpp::TempoSessionProvider` owns channel recovery and cumulative voucher state.
Its `ChannelStore` is deliberately persistence-only. Nanocodex opts into the
MPPx-compatible SQLite implementation at `~/.tempo/wallet/channels.db`; a
library caller that does not configure persistence gets the in-memory store.
Before the paid WebSocket probe, the provider performs the server's
authenticated `HEAD` bootstrap, reconciles the returned session snapshot with
TIP-1034 on-chain state, and then updates SQLite and its live registry. A new
process can consequently resume a server-known session even when its local
database starts empty.

The OpenAI proxy offers native v2 before legacy v1. The v2 challenge uses:

- Moderato chain ID `42431` or Tempo mainnet chain ID `4217`;
- TIP-1034 reserve precompile
  `0x4d50500000000000000000000000000000000000`;
- cumulative off-chain vouchers;
- optional server fee sponsorship.

`TempoSessionProvider::voucher_credential` returns a signed credential for
in-band transports without performing an unrelated HTTP POST. Its existing SSE
helper delegates to that method and then performs the POST.

Native session dependencies require Rust 1.93, newer than Nanocodex's Rust
1.88 library baseline. The executable declares that higher MSRV while the MPP
integration stays out of the library crates and their dependency graph.

## OpenAI proxy

`mpp-proxy` adds `GET /v1/responses` as a paid WebSocket endpoint while keeping
the existing HTTP POST route. The GET without an upgrade is the HTTP payment
probe. The upgrade path:

- uses `mppx`/`tempo.Ws.serve` for canonical session authorization and voucher
  verification;
- opens an outbound WebSocket to `https://api.openai.com/v1/responses`;
- accepts only `response.create` application frames;
- incrementally tokenizes output text with `o200k_base`, retaining a conservative
  128-token suffix so later deltas cannot change an already charged boundary;
- calls `stream.charge(incremental_output_amount)` before releasing each output
  delta, causing mppx to request the next cumulative native session voucher;
- forwards the caller's request and OpenAI events byte-for-byte, so the
  Nanocodex wire stream and retained history remain unchanged;
- prices the terminal event from OpenAI's authoritative usage counters;
- accounts independently for uncached input, cached input, cache writes,
  visible and reasoning output, the 272K long-context tier, service tier, and
  supported paid tools;
- charges only the remaining difference before releasing the terminal event;
- allows one response in flight and resets that state only on an OpenAI
  terminal event;
- reuses the upstream WebSocket for every sequential response;
- caps queued upstream data at 64 MiB while a voucher is being obtained;
- observes mppx's cancellation signal so native session close cannot deadlock
  behind an idle application generator.

Output remains live: only a delta that makes a sufficiently old group of output
tokens stable pauses for payment, and that delta is released as soon as its
cumulative voucher is verified. The retained tokenizer suffix is charged during
terminal reconciliation. OpenAI does not stream input, cache-write, hidden
reasoning, paid-tool, or final long-context accounting as individual tokens, so
the authoritative terminal usage event reconciles those amounts without double
charging. The persistent WebSocket and retained response chain are unchanged.

For `gpt-5.6-sol`, standard short-context prices per million tokens are $5
uncached input, $0.50 cached input, $6.25 cache writes, and $30 output. The
resolver also contains the published Terra and Luna tables, Flex and Priority
tiers, long-context multipliers, and Web/File Search call fees. Amounts round
up to the proxy currency's one-micro-dollar atomic unit.

MPP sessions require a positive base tick, so the route advertises one atomic
unit. Every actual OpenAI request supplies its explicit dynamically calculated
amount; the base tick is not used as the request price.

Dynamic WebSocket charging requires `mppx`'s session controller to accept an
optional explicit amount. Calling `charge()` without an amount preserves the
existing fixed-tick behavior.

## CLI

Enable the adapter with:

```text
--provider.openai
--provider.tempo
--provider.tempo.egress                    # opt-in HTTP(S) tool egress
--provider.tempo.responses-websocket-url <ws-or-wss-url>
--provider.tempo.wallet-store <path>       # default ~/.tempo/wallet/store.json
--provider.tempo.channel-store <path>      # default ~/.tempo/wallet/channels.db
--provider.tempo.rpc-url <url>
--provider.tempo.max-deposit <atomic-units>
--provider.tempo.egress-max-charge <atomic-units> # default 100000 ($0.10 USDC.e)
--provider.tempo.api-key <key>             # optional gated deployment key
```

These are global CLI flags. They select the same paid transport for both the
interactive TUI and the headless one-shot runner:

```text
nanocodex --provider.tempo --prompt "say hello"
nanocodex run "say hello" --provider.tempo
```

The defaults target Tempo mainnet and
`wss://openai.mpp.tempo.xyz/v1/responses`. Direct OpenAI is the default;
`--provider.openai` makes that selection explicit. `--provider.tempo` does not
require an OpenAI API key because the proxy owns the upstream credential.

The eventual login surface follows the same namespace: `nanocodex login`
defaults to OpenAI OAuth, while `nanocodex login --provider.tempo` runs the
Tempo Wallet login flow and writes the shared Accounts SDK store. OpenAI OAuth
is intentionally not stubbed by this integration.

Both paths retain the adapter until the agent handle is dropped and then
perform the canonical signed session close. TUI teardown restores the terminal
before waiting for that network handshake.

## HTTP(S) tool egress

`--provider.tempo.egress` starts a private HTTP forward proxy on an ephemeral
loopback port. Nanocodex injects authenticated `HTTP_PROXY`/`HTTPS_PROXY`
variants and an ephemeral CA path into workspace-tool child processes only.
It clears inherited proxy-bypass lists for those children, but does not change
the parent process environment, the model WebSocket, web search, image
generation, or MCP transports. Ordinary commands such as `curl` therefore need
no MPP-specific wrapper:

```text
nanocodex --provider.tempo --provider.tempo.egress \
  --prompt "curl the paid endpoint and summarize the response"
```

For HTTPS, the proxy terminates the child's TLS connection with the ephemeral
CA, forwards the request with redirects disabled, and streams the final
response back. It buffers each request body up to 16 MiB so the exact method,
headers, and body can be replayed after a valid MPP 402 challenge. At most four
payment challenges are accepted per request. Protocol upgrades are tunneled
without MPP handling.

The signing provider and wallet material remain inside the binary. The
loopback proxy rejects requests without a random per-process credential, which
is injected only into tool children. Its CA and proxy credential disappear
when the adapter is dropped.

The proxy library is generic over `mpp::client::PaymentProvider`, so it is not
tied to curl, Code Mode, Tempo, or a particular MPP intent. The current binary
wires in both Tempo charge and session providers. One-shot `tempo.charge`
challenges are rejected above `--provider.tempo.egress-max-charge`; the default
ceiling is 100,000 atomic units ($0.10 for USDC.e). Session deposits remain
bounded by `--provider.tempo.max-deposit`. Unsupported, over-limit, or malformed
402 challenges fail the proxied request rather than being paid blindly.

Each proxied request emits a correlated `mpp.egress.request` span. Its ordered
events record the original request, selected 402 challenge, created credential,
paid replay status, final response headers, and streamed response body. Persist
the record as structured JSON with:

```text
nanocodex run "curl the paid endpoint" \
  --provider.tempo --provider.tempo.egress \
  --log-format json --log-file .nanocodex/logs/mpp-egress.jsonl
```

These traces are full-fidelity operational records. They contain request and
response content plus payment credentials, so operators must protect and retain
them like the corresponding wallet and agent conversation data.

The endpoint is configurable, so the same transport can call other compatible
MPP WebSocket services. A local proxy uses a service subdomain, for example
`ws://openai.localhost:8787/v1/responses`. Discovery remains a caller/tool
concern; the egress proxy only makes ordinary HTTP clients payment-aware.

## Validation

Required validation before merging:

- `mppx`: explicit dynamic charge reservation/commit test.
- `mpp-rs`: cumulative native voucher credential test and canonical
  probe/authorization/application-frame integration test.
- `mpp-proxy`: format, lint, typecheck, full unit suite, and an OpenAI WS bridge
  test.
- Nanocodex: rustfmt, Clippy with warnings denied, CLI tests, and unchanged
  library tests.
- Live: use the logged-in mainnet wallet access key, rehydrate or open a native
  v2 channel, and complete two sequential real OpenAI Responses turns through
  one paid WebSocket without exposing either the wallet JWK or OpenAI secret.
