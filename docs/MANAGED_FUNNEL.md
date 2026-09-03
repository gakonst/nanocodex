# Hosted homepage demo policy

## Product boundary

The deployment owner may sponsor model access only for the SMS-verified
homepage browser demo. The journey is:

```text
SMS verification -> three free ephemeral prompts -> connect or fund Wallet
```

The sponsored demo gives each persistent SMS account exactly three prompts. It
uses `gpt-5.6-luna` with thinking disabled. Its browser agent sets
`durability: false`, creates a fresh thread ID for the mounted page, and does not
expose conversation history. Reloading discards the model thread but does not
reset the three-prompt allowance.

When signed out, the homepage terminal itself renders the phone and one-time
code steps in its composer area. Successful verification replaces that form
with the ordinary prompt composer; users do not have to discover the account
menu before starting the trial.

The Durable Agent page is a separate product boundary. It requires the account's
own connected ChatGPT subscription or OpenAI API key. The shared deployment-owner
credential cannot create, open, resume, or fund a durable agent. A future paid
Nanocodex plan may satisfy that gate through its own explicit entitlement, but a
MACH wallet balance alone is not model authority.

Chief of Staff, Attached Tools, Multiplayer, World, and other managed-agent
surfaces are not sponsored by this homepage policy.

## Credential routing

The deployment owner signs into an ordinary Nanocodex account and connects
ChatGPT through the existing Account surface. Egress retains that credential in
the owner's encrypted `UserCredentialBroker`; it is not copied into demo users.
Set `NANOCODEX_SPONSORED_CHATGPT_USER_ID` on the private egress Worker to that
stable Nanocodex account ID.

For model traffic, egress applies this precedence:

1. use the requesting account's connected ChatGPT or OpenAI credential;
2. if none exists, allow the deployment owner's ChatGPT credential only when the
   subject is the exact 43-character browser-model identity;
3. fail closed for the 64-character Durable Object identity used by managed
   agents.

The status response exposes only `source: "user"` or `source: "sponsored"` and,
for sponsored access, the number of free prompts remaining.
The sponsor's account ID, ChatGPT account ID, access token, and refresh token do
not cross the egress boundary. Refresh remains centralized in the sponsor's
credential broker.

## Allowance enforcement

The requesting account's credential Durable Object keeps the three accepted
root prompt IDs. Reservations are serialized, so concurrent tabs cannot exceed
the allowance. The egress Worker reserves and marks a root prompt before
forwarding an actual Responses WebSocket generation. A completed root cannot
be replayed. Each live attempt renews a broker-owned lease; an interrupted or
orphaned in-flight root gets at most one transport retry, and stale attempt
events cannot mutate its replacement.
Provider-issued tool and tool-search continuation grants are single-use and
survive the SDK's full-history WebSocket reconnect without consuming another
prompt. Warmup frames do not consume a slot. Sponsored frames are canonicalized
at egress to Luna, no thinking, and standard service before they reach the
provider.

After the third prompt completes, the homepage replaces the composer with
`Connect` and `Fund Wallet` actions. Connect leads to the user's model
connections; Fund Wallet leads to the existing dollar-denominated onramp. The
server rejects a fourth distinct prompt even if a stale or modified client
still submits it. Funding is presented separately and does not itself grant
model authority in this iteration.

## Deliberate non-goals

This policy has no passkey grant, OpenAI MPP client, MACH payer switch, or
durable sponsored thread. MACH wallet and onramp features remain separate until
an explicit paid-agent entitlement is designed and implemented.
