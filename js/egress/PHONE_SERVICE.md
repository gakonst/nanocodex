# Dedicated inbound phone service

`phone-service.ts` provides long-lived, account-owned US local SMS numbers. It never sends SMS, automatically releases a number, or promises that an external service accepts VoIP numbers for verification. Releasing a number can permanently break account recovery; the owner must confirm release in the authenticated account UI.

The private egress entry point is `https://phone-service.internal/v1/users/:owner/...`. The account and Connect gateways determine the owner from authenticated credentials. Account-facing routes are under `/v1/services/phone`. Connect can create an intent and poll its receipt within a grant; it cannot approve purchases or releases. Only the account gateway may insert `x-nanocodex-phone-human-approval: true` after verifying an account session and the exact request Origin.

## Configuration

`PHONE_SERVICE_ACCOUNTS` binds the `PhoneServiceAccount` SQLite Durable Object class. Account objects own receipts, number metadata and encrypted messages. Separate number objects enforce exclusive allocation across accounts and retain retired-number tombstones. A retired number is never reassigned through this service, preventing old signed webhook replays from crossing tenants.

Production uses the private `TWILIO_PHONE_PROVIDER` service binding to the managed Worker's `PhoneProvider` entry point. Existing managed Twilio credentials remain there. The provider accepts only number search, pricing, purchase, resource inspection and release operations; it exposes neither credentials nor arbitrary provider requests. Direct `TWILIO_ACCOUNT_SID` / `TWILIO_AUTH_TOKEN` bindings are supported for isolated deployments and tests. The auth token may be a Secrets Store binding.

Required configuration:

| Binding | Meaning |
| --- | --- |
| `CREDENTIAL_ENCRYPTION_KEY` | Existing AES-GCM vault key; SMS content uses account-specific authenticated encryption. |
| `PHONE_WEBHOOK_URL` | Exact HTTPS URL ending `/v1/services/phone/webhook`, without query or fragment. Must match the published account ingress. |
| `PHONE_PROVISIONING_ENABLED` | Must be `true` before an intent or purchase can proceed. Defaults to disabled. |
| `PHONE_MAX_MONTHLY_PRICE` | Positive operator ceiling for the monthly number rental, in USD. Required to provision. |
| `PHONE_MAX_INBOUND_SMS_PRICE` | Positive operator ceiling for the provider's per-segment inbound SMS price, in USD. Required to provision. |
| `PHONE_MAX_NUMBERS_PER_OWNER` | Active and uncertain allocation limit, default 3, maximum 10. |
| `PHONE_MESSAGE_TTL_SECONDS` | Local encrypted inbox retention, default 86400, range 1–86400. This does not configure Twilio's independent retention. |
| `PHONE_MAX_MESSAGE_RECEIPTS` | Lifetime SID deduplication ceiling per account, default/maximum 10000; lower positive limits are supported. At capacity new messages fail closed. |

The checked-in deployment keeps provisioning disabled and does not supply price ceilings. Configure actual ceilings deliberately before enabling an operator-billed service. Limits bound local actions and storage; they are not a carrier-level spending cap, because inbound traffic can already have incurred provider charges before the webhook arrives. Use the provider's account billing controls as appropriate.

## HTTP contract

All JSON responses have `Cache-Control: no-store`. IDs are lowercase UUIDs; timestamps are ISO 8601. Monetary values are decimal strings with an ISO currency code. Initial provisioning is limited to US local SMS-capable numbers without address requirements; other countries fail closed.

| Method and suffix | Request | Response |
| --- | --- | --- |
| `GET /numbers/available` | Query `country=US`, optional `area_code`, `limit` 1–20 | `{numbers:[{phone_number,country,type:"local"}]}` |
| `GET /numbers` | — | `{numbers:[{id,phone_number,country,status,created_at}]}` |
| `POST /numbers` | `{operation_id,phone_number,country:"US"}` | `{request}`; creates a quote and intent only |
| `GET /numbers/:id` | — | `{number}` |
| `DELETE /numbers/:id` | `{operation_id}` | `{request}`; creates a release intent only |
| `GET /numbers/:id/messages` | Optional `limit` 1–50 and opaque `cursor` | `{messages:[{id,from,to,body,received_at,expires_at}],next_cursor}` |
| `GET /requests/:operation_id` | — | `{request}`; safely reconciles uncertain outcomes using provider reads |
| `POST /requests/:operation_id/approve` | Purchase: `{quote_id,accept_recurring:true}`; release: `{confirm_release:true}` | `{request}`; trusted human approval required |
| `POST /requests/:operation_id/deny` | `{}` | `{request}`; trusted human decision required |

A request contains `operation_id`, `kind` (`purchase` or `release`), `status`, `phone_number`, `created_at`, and optional `number_id`, `quote`, or a stable `error` code. Status is `pending_approval`, `complete`, `denied`, `expired`, `failed`, or `outcome_unknown`. A purchase quote contains `id`, `currency`, `monthly_price`, `inbound_sms_price`, `recurring:true`, and `expires_at`. The initial monthly rental is billed at purchase and recurs until release; inbound usage is additional. The quote expires after ten minutes, and approval rechecks both rental and inbound prices. A price change requires a new intent and human review.

The same operation ID and input retrieve its durable receipt. Different inputs conflict. Before purchase or release is sent, the object stores an `outcome_unknown` fence. Neither retries nor new IDs can repeat an uncertain allocation or release. GET reconciliation only adopts a unique provider purchase whose number, account, UUID-based FriendlyName, and webhook configuration all match; release reconciliation requires Twilio's missing-resource response (`404`, code `20404`). If evidence is absent or ambiguous, the operation stays unknown. Operator investigation may be necessary; there is no automatic compensating release or re-purchase.

## Inbound messages and bounds

The public ingress is `POST /v1/services/phone/webhook`; egress normalizes it to `/v1/phone/webhook`. Validation uses the exact configured canonical URL, the full form parameter set and Twilio's HMAC-SHA1 signature. Duplicate form names, a different account, unknown destination, inactive ownership, oversized data, or media are rejected. A valid callback returns empty TwiML and never sends a reply.

Message bodies and sender information are encrypted at rest. Expired messages are excluded from reads and removed by reads/alarms. The account inbox retains at most 200 messages; paging is bounded at 50. Permanent SID-only receipts prevent signed replay after message expiry. Other limits are 120 API requests/minute/account, 20 provider read/intent operations/minute/account, 60 unique inbound messages/minute/account, 1000 lifetime operation fences and 100 lifetime number records. Capacity errors preserve fences instead of silently deleting ownership history. No quota action releases an allocated number.

## Validation

Run from the repository root:

```sh
node --test js/egress/test/phone-service-journey.node.test.mjs
pnpm --dir js/egress typecheck
```

The journey bundles the production egress Worker and runs HTTP requests against workerd with persistent SQLite storage. It runs once with direct synthetic provider credentials and once through the actual managed `PhoneProvider` binding. Only the remote Twilio provider is synthetic. It exercises quote/approval, price change, denial, ownership isolation, signatures, encryption, expiry, pagination, resource limits, missing configuration, duplicate operations, unknown outcomes, read-only reconciliation, and process restart. Traces and databases are written under ignored `output/phone-service-journey/{direct,broker}/`. No live purchase, release, message send or deployment occurs. Account-session/Connect authorization is additionally validated in the gateways' own transport journeys.

Provider references: [phone-number pricing](https://www.twilio.com/docs/phone-numbers/pricing), [inbound messaging pricing](https://www.twilio.com/docs/messaging/api/pricing), [number search](https://www.twilio.com/docs/phone-numbers/api/availablephonenumberlocal-resource), [purchase/release resource](https://www.twilio.com/docs/phone-numbers/api/incomingphonenumber-resource), [webhook signatures](https://www.twilio.com/docs/usage/security), [monthly billing](https://help.twilio.com/articles/223182908).
