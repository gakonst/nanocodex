# MPP Charge integration

## Boundary

MPP remains an application concern. The private
`bin/nanocodex/src/mpp/egress.rs` module composes with the normal Nanocodex
HTTPS Responses client. No public Nanocodex library crate contains wallet or
payment behavior.

The Tempo provider pays Responses with estimated, up-front `tempo/charge` over
HTTPS. It deliberately does not configure an MPP WebSocket transport for the
model. In Tempo provider mode, Nanocodex also enables its built-in Mercator MCP;
that protocol-level client supports both `tempo/charge` and `tempo/session`
challenges from Mercator and services composed behind it.

```text
Nanocodex Responses HTTPS client
              |
     loopback MPP egress
              |
       HTTP 402 challenge
              |
   MPP TempoAccountsProvider
              |
 tempo-alloy Accounts wallet
              |
       NanoUSD payment
              |
     exact request replay
              |
      streamed SSE response
```

The proxy, not Nanocodex, owns the upstream OpenAI credential. Nanocodex sends
the Responses request, `Accept-Payment: tempo/charge`, and an optional
deployment access key.

## Accounts and signing

`mpp-rs` opens the Tempo Accounts SDK store at
`~/.tempo/wallet/store.json` by default. Its `TempoAccountsProvider` uses the
concrete `TempoAccountsWallet` from `tempo-alloy`, which lazily selects an
authorized access key when a payment transaction is prepared. The resulting
`TempoAccessKey` pins that exact key through authorization resolution, gas
filling, sponsorship, and signing.

There is no Nanocodex wallet wrapper, signer enum, or signing-mode flag.
`tempo-alloy` implements Alloy's existing wallet and filler traits;
`mpp-rs` owns Challenge decoding, Charge transaction construction, optional
fee sponsorship, settlement-RPC selection from the challenge chain, and
receipt lifecycle; Nanocodex only supplies application policy.

NanoUSD on Tempo mainnet is the configured payment input. A service that
charges NanoUSD is paid directly. For another supported stablecoin challenge,
MPP may use the Stablecoin DEX to swap from NanoUSD within the configured
slippage bound.

## CLI

Release and nightly artifacts include Tempo support. Source builds opt in so
the direct-agent development loop does not compile the payment stack:

```console
cargo build -p nanocodex-bin --bin nanocodex --features tempo
```

Enable paid Responses and paid HTTP tool egress with:

```text
nanocodex run "say hello" --provider.tempo
nanocodex --provider.tempo --prompt "say hello"
```

Relevant global options:

```text
--provider.openai
--provider.tempo
--provider.tempo.api-base-url <https-url>  # default https://openai.mpp.tempo.xyz/v1
--provider.tempo.wallet-store <path>       # default ~/.tempo/wallet/store.json
--provider.tempo.payment-token <address>   # default NanoUSD
--provider.tempo.swap-slippage-bps <bps>   # default 100
--provider.tempo.api-key <key>             # optional gated deployment key
```

The payment token is both the preferred challenge currency and the input token
for automatic swaps when a service charges another supported currency. For
example, use PathUSD with:

```console
nanocodex --provider.tempo \
  --provider.tempo.payment-token 0x20c0000000000000000000000000000000000000
```

It can also be set with `NANOCODEX_PROVIDER_TEMPO_PAYMENT_TOKEN`.

Tempo selects the HTTPS Responses transport. Explicitly selecting WebSocket
with `--provider.tempo` is rejected during startup. Direct OpenAI continues to
default to its persistent Responses WebSocket.

## Built-in Mercator MCP

`--provider.tempo` adds `https://mercator.tempo.xyz/mcp` to the standard MCP
defaults. Direct OpenAI mode does not add it. `--mcp-defaults=false` is the
explicit opt-out, while a named `mercator` entry in Codex config or `--mcp`
overrides the built-in endpoint.

Mercator remains deferred: the model sees provider-native `tool_search`, then
calls discovered `mcp__mercator__*` functions only from Code Mode. On MCP error
`-32042` or `org.paymentauth/payment-required` result metadata, the mpp-rs client
selects a supported unexpired Tempo challenge and creates a credential with the
same Accounts wallet and access-key policy as the model provider. Nanocodex
places it in `org.paymentauth/credential` and retries the tool call. Successful
calls commit the authorization; definitive MCP rejection rolls it back;
ambiguous transport failure preserves durable payment state.

Session channels use the shared Tempo SQLite channel store, scoped to the MCP
endpoint, with a 0.05-token maximum reserve and top-up. This allows a single
Mercator integration to satisfy either one-time Charge or high-volume Session
MPP without exposing wallet material to the model or MCP server.

Charge payment is accepted before the complete SSE response has arrived, so
the CLI limits paid Responses calls to one SDK attempt. A premature close or
other retryable stream failure is returned to the caller instead of replaying
the request automatically. Retrying that prompt is an explicit caller action.

The API base must use HTTPS. Plain HTTP is accepted only for loopback
development endpoints, including an SSH-forwarded service. The
Tempo-specific `--provider.tempo.api-base-url` takes precedence over the
generic OpenAI API base setting while the Tempo provider is enabled.

## HTTP tool egress

`--provider.tempo` also starts a private HTTP forward proxy on an ephemeral
loopback port. Nanocodex routes its own Responses and remote-tool clients
through that proxy and gives authenticated proxy environment variables plus an
ephemeral CA to workspace-tool child processes. It does not mutate the parent
environment. MCP protocol payments are handled separately at the MCP client
boundary because their challenges are JSON-RPC errors rather than HTTP 402
responses.

The egress proxy buffers a request body up to 16 MiB so it can replay the exact
request after a valid 402 challenge. It rejects redirects, protocol upgrades,
unsupported payment methods, and malformed challenges. The wallet and signing
key never leave the Nanocodex process.

MPP tracing records full request, response, challenge, and credential content.
Operators must protect those traces like wallet and conversation data.

### Agent-driven host and VM smoke

This exercises the complete path through Nanocodex and Code Mode rather than
calling the proxy on its own. The model fetches the live public catalog at
`https://mpp.dev/api/services`, chooses endpoints within the configured charge
cap, and fans curl subprocesses out from one `Promise.all` cell:

```sh
nanocodex run --provider.tempo \
  "Use one Code Mode cell. Fetch https://mpp.dev/api/services, select eight safe HTTP service smoke requests with low advertised charges, then use Promise.all over exec_command calls that each run curl -fsS. Verify every exit code and report the endpoint/status matrix."
```

Run the same model turn with every workspace command inside the retained VM:

```sh
nanocodex run --provider.tempo \
  --vm .nanocodex/vm/session-rootfs.ext4 \
  --vm-guest-runtime target/aarch64-unknown-linux-musl/debug/nanocodex-vm-guest \
  "Use one Code Mode cell. Fetch https://mpp.dev/api/services, select eight safe HTTP service smoke requests with low advertised charges, then use Promise.all over exec_command calls that each run curl -fsS. Verify every exit code and report the endpoint/status matrix."
```

Host mode applies the proxy route only to tool subprocesses. VM mode projects
the same authenticated URL and public ephemeral CA through an `EgressLease`
before tools become available; it does not put host filesystem paths or wallet
material in the guest. Both modes keep model Responses traffic in the host
process while routing its HTTPS client through the same Tempo payment layer.

## Credits onramp

`nanocodex credits` and `nanousd-api` are separate from MPP settlement:

1. the CLI reads the same Accounts SDK wallet and requests a credit package;
2. Stripe Checkout and its signed webhook establish successful payment;
3. `nanousd-api` issues the package's NanoUSD to the account on Tempo mainnet;
4. later MPP Charge requests spend those NanoUSD credits.

The service binds to `127.0.0.1:8789` by default. During development it can stay
private behind an SSH tunnel; a later Tailscale Funnel can expose the same
HTTP service after both host and SentinelOne firewall rules are approved.

## Validation

Before merging:

- run rustfmt, Clippy with warnings denied, and the focused Nanocodex tests;
- run MPP's minimal `client,tempo` build and sponsored Charge tests;
- complete a live HTTPS Responses request whose 402 challenge names NanoUSD;
- retain the MPP receipt and verify the wallet's NanoUSD debit;
- create a Stripe Checkout order, deliver its signed webhook, and verify the
  corresponding NanoUSD issuance transaction.
