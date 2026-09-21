# Standalone inference API

Use the standard OpenAI SDK Responses interface with this base URL:

```text
https://nanocodex.gakonst.workers.dev/v1
```

Every credential and identifier in the examples is a placeholder. Obtain an inference key from the deployment operator before making requests.

### Terminal demo

Download `inference-demo.sh` from the companion Gist, or run `bash scripts/inference-demo.sh` from the repository. Requires Bash 3.2 or newer, curl, and jq. It prompts privately for an inference key when `NANOCODEX_INFERENCE_KEY` is unset:

```sh
bash inference-demo.sh
```

The demo lists the live model/provider/effort catalog and runs six varied prompts through `model: "auto"`. Each prompt is a separate stateless request, so each receives a fresh routing decision. It prints the generated answer, Jev diagnostics exposed by the server, selected provider/model/effort, router duration, HTTP timing, and token usage, followed by a comparison table. It does not execute tools or use connectors or Hands.

```sh
# Catalog only; no generation quota consumed.
bash inference-demo.sh --models-only

# Custom questions; repeat --prompt to compare independent routes.
bash inference-demo.sh --prompt 'Explain binary search in two sentences.' \
  --prompt 'Find the bug: function sum(a) { return a.reduce((x,y) => x+y); }'

# Save request/response JSON and results locally for inspection.
bash inference-demo.sh --output ./inference-results
```

Use `--help` for all options. The default run makes six generation requests, which count toward the key's quota and use provider credits. A failed or ambiguous request is never automatically retried. Saved output contains your prompts and generated answers; credentials are excluded.

Jev choice confidence describes routing/classification confidence, **not task-success probability**. Missing distributions stay unavailable; the demo never creates a probability distribution from a winning choice's confidence. HTTP time to first byte measures this buffered API's delivery, **not model generation TTFT**. Check response status: a successful HTTP request may still contain an incomplete generation.

### Python (OpenAI SDK)

Install `openai` and set `NANOCODEX_INFERENCE_KEY` in your environment:

```python
import os
from openai import OpenAI

client = OpenAI(
    api_key=os.environ["NANOCODEX_INFERENCE_KEY"],
    base_url=os.environ.get("NANOCODEX_BASE_URL", "https://nanocodex.gakonst.workers.dev/v1"),
    max_retries=0,
)
response = client.responses.create(
    model="auto",
    input="Explain why the sky is blue in two sentences.",
    max_output_tokens=512,
)
print(response.output_text)
```

### JavaScript (OpenAI SDK)

Install `openai`, set the same environment variable, and save as an `.mjs` file:

```js
import OpenAI from "openai";

const client = new OpenAI({
  apiKey: process.env.NANOCODEX_INFERENCE_KEY,
  baseURL: process.env.NANOCODEX_BASE_URL ?? "https://nanocodex.gakonst.workers.dev/v1",
  maxRetries: 0,
});
const response = await client.responses.create({
  model: "auto",
  input: "Explain why the sky is blue in two sentences.",
  max_output_tokens: 512,
});
console.log(response.output_text);
```

### Raw HTTP

```sh
export NANOCODEX_INFERENCE_KEY='YOUR_INFERENCE_KEY'
curl --fail-with-body 'https://nanocodex.gakonst.workers.dev/v1/responses' \
  -H "Authorization: Bearer $NANOCODEX_INFERENCE_KEY" \
  -H 'Content-Type: application/json' \
  --data '{"model":"auto","input":"Explain why the sky is blue in two sentences.","max_output_tokens":512}'
```

These requests are **stateless**: omit `session_id`, send all history needed for each call, and expect a fresh routing decision on each `model: "auto"` request. Calls do not pin a route across requests. For provider/model/effort pinning and routing preferences, use the optional session extension below. `previous_response_id` is unsupported; the server does not restore conversation history.

The service implements a **bounded, text-only Responses-format subset with custom routing/session extensions**, not full OpenAI API parity. `stream: true` returns **buffered SSE after upstream generation finishes**, not live token delivery. The SDK examples disable automatic retries because generation has no replay guarantee. The SDK's `output_text` convenience accessor reads message output; raw JSON clients should read `output` items.

Inference uses deployment-funded Workers AI, OpenRouter, or Vercel AI Gateway. Inference credentials do not authorize account data, memories, connectors, Hands, shell execution, browsers, ChatGPT subscriptions, or server tool execution. Function and custom tools describe calls for your application to handle; the service only returns the calls as data. Inference keys are owner-attributed and retain only inference authority.

## Authentication

Use an inference key, whose prefix is `nci_live_`:

```http
Authorization: Bearer YOUR_INFERENCE_KEY
```

The bearer scheme and token must match the issued value. Cookies and full-account `ncx_live_` keys do not authenticate inference requests. Inference keys cannot authenticate account endpoints or manage keys, including when accompanied by account cookies. Each key is attributed to its issuing user account in server-side ownership records and carries immutable `scope: "inference"`. Owner attribution does not grant account capabilities: it never enables that user’s connectors, Hands, saved data, or subscription credentials. These are two authority classes: `ncx_live_` account keys retain their existing account permissions, while `nci_live_` platform inference keys authorize only this inference API. A platform inference key never inherits its owner’s account permissions and cannot change or widen its scope. Do not include query parameters on inference API URLs.

Use `NANOCODEX_BASE_URL` for the standard SDK base (`/v1`) and `NANOCODEX_INFERENCE_BASE` for the optional extension (`/v1/inference`), alongside `NANOCODEX_INFERENCE_KEY`. These are also the names used in a delivered private `.env` file. Keep keys in your application server's environment or a local environment variable. The admin key used for issuance is a separate, more powerful credential and must not be distributed to inference users.

```sh
export NANOCODEX_BASE_URL='https://nanocodex.gakonst.workers.dev/v1'
export NANOCODEX_INFERENCE_BASE="$NANOCODEX_BASE_URL/inference"
export NANOCODEX_INFERENCE_KEY='YOUR_INFERENCE_KEY'
```

## Endpoints

Paths below are relative to the standard `/v1` base URL. `/v1/inference/models` and `/v1/inference/responses` remain supported aliases with the same authentication and request contract. Only the exact `/v1/models` and `/v1/responses` paths are standard aliases; they do not grant access to any other `/v1` route.

| Method | Path | Authorization | Result |
| --- | --- | --- | --- |
| GET | `/models` | Inference key | Eligible deployment-funded routing candidates. |
| POST | `/inference/sessions` | Inference key | Create a session; HTTP 201. |
| GET | `/inference/sessions/{session_id}` | Same inference key | Session metadata. |
| DELETE | `/inference/sessions/{session_id}` | Same inference key | Delete session; HTTP 204. |
| POST | `/responses` | Inference key | Stateless generation, or an optional owned `session_id`. |
| GET | `/inference/keys` | Deployment operator, full account, `api_keys:read` | List this account's inference-key metadata. |
| POST | `/inference/keys` | Deployment operator, full account, `api_keys:write` | Issue one inference key; HTTP 201. |
| DELETE | `/inference/keys/{key_id}` | Deployment operator, full account, `api_keys:write` | Revoke an owned key; HTTP 204. |

Key administration is restricted to the deployment operator whose account matches the configured `NANOCODEX_ADMIN_USER_ID`; other accounts are denied even with key-management capabilities. An unset operator ID fails closed. Teammates use their issued inference keys for the data endpoints.

The data-plane deployment gate is `NANOCODEX_INFERENCE_ENABLED=true`; an unavailable service returns HTTP 503. Admin key routes are separate from that gate.

## Discover models

```sh
curl --fail-with-body "$NANOCODEX_BASE_URL/models" \
  -H "Authorization: Bearer $NANOCODEX_INFERENCE_KEY"
```

The result is an object with `object: "list"` and a `data` array. Each entry has the model-list fields `id`, `object: "model"`, `created: 0`, and `owned_by` (the provider), plus the routing extensions `model`, `provider_model`, `provider`, and `thinking`. `created: 0` is a placeholder, not a model release timestamp. Example entry:

```json
{
  "id": "@cf/zai-org/glm-5.3:medium",
  "object": "model",
  "created": 0,
  "owned_by": "workers_ai",
  "model": "@cf/zai-org/glm-5.3",
  "provider_model": "@cf/zai-org/glm-5.3",
  "provider": "workers_ai",
  "thinking": "medium"
}
```

Cloudflare frontier entries use `provider: "cloudflare"` and candidate IDs such as `cloudflare:openai/gpt-6-astra:low`. Astra, Sol, Terra, and Luna each support `low`, `medium`, and `high` routing candidates. These use Cloudflare's native Responses API through deployment-owned Cloudflare billing; they need no user connector or separate OpenAI key. They appear only when `NANOCODEX_CLOUDFLARE_FRONTIER_ENABLED=true` and a valid Cloudflare transport is configured. The gate defaults off when omitted. Cloudflare-hosted GLM retains its existing `workers_ai` identity. Existing sessions keep their exact provider/model/effort pins; disabling Cloudflare frontier access makes a pinned Cloudflare request unavailable rather than switching its provider.

Deployment operators can select the Cloudflare REST transport by setting both `NANOCODEX_CLOUDFLARE_ACCOUNT_ID` (the 32-character account ID) and the Worker secret `CLOUDFLARE_AI_API_TOKEN` (a deployment-owned Cloudflare token authorized for inference on that account). Requests go only to `https://api.cloudflare.com/client/v4/accounts/<account-id>/ai/v1/responses`. This credential is separate from the `nci_` keys distributed to API callers. It is never returned in API responses, persisted in session metadata, or included in routing telemetry.

When neither REST setting is present, the existing AI binding transport remains unchanged. Supplying either setting selects REST exclusively: incomplete or invalid configuration makes Cloudflare unavailable, including for already pinned sessions, without falling back to the binding or another provider. Main threads and subagents share this transport selection while retaining their own exact provider/model/effort pins. Enabled periodic TTFT probes use the same selected transport and existing request budgets; configuring REST does not enable probes.

OpenRouter and Vercel gateway entries appear only when their deployment credentials are configured. Read the live catalog for available IDs. Native ChatGPT subscription candidates are excluded. `model: "auto"` lets the router choose from eligible candidates. You can also pass a catalog entry's canonical `model` to restrict selection to that model, or its exact `id` to select a particular provider/model/effort candidate. Only eligible deployment-funded candidates are accepted; unavailable and native ChatGPT subscription models are excluded. A canonical model can have multiple candidates; use the exact candidate ID when provider and effort must be fixed.

## Optional session extension: create and inspect a session

Use this extension when repeated calls should retain routing preferences and a provider/model/effort pin. **Your application still sends the full conversation history on every response request.** A session stores routing metadata and counters; it is not a managed Nanocodex agent. Without `session_id`, neither SDK calls nor raw HTTP calls use a persistent session.

A minimal creation body is `{}`. Only the optional `routing` property is accepted.

```sh
SESSION_JSON=$(curl --fail-with-body "$NANOCODEX_INFERENCE_BASE/sessions" \
  -H "Authorization: Bearer $NANOCODEX_INFERENCE_KEY" \
  -H 'Content-Type: application/json' \
  --data '{"routing":{"preferences":{"completion":60,"cost":25,"duration":15}}}')
SESSION_ID=$(printf '%s' "$SESSION_JSON" | jq -r '.id')
printf '%s\n' "$SESSION_JSON"
```

The metadata has `id`, `key_id`, normalized `routing`, `route`, and `counters` (`requests`, `completed`, `failed`). `route` is initially `null`. The public `id` is the UUID to send as `session_id`.

```sh
curl --fail-with-body "$NANOCODEX_INFERENCE_BASE/sessions/$SESSION_ID" \
  -H "Authorization: Bearer $NANOCODEX_INFERENCE_KEY"
```

Sessions belong to the exact issuing inference key. A different key, even from the same account, cannot read, continue, or delete that session. A session GET does not return conversation history or generated responses.

### Routing policy

The direct router chooses a candidate from the first response request's opening input and pins it before generation. Subsequent requests retain that provider, canonical model, and thinking effort, including after generation failure or restart. No session-update or rerouting endpoint exists; create a new session for a new decision.

The policy uses the shared thread-routing schema but removes native ChatGPT candidates and supports only the direct strategy. An explicit `strategy: "legacy"` is rejected. Prefer the following common options:

| Field | Meaning |
| --- | --- |
| `candidates` | Nonempty array of exact candidate IDs from `/models`; constrains provider/model/effort. |
| `preferences.completion`, `.cost`, `.duration` | Optional relative importance from 0–100; higher cost/duration weights favor lower spend/less elapsed time. |
| `preferences.text` | Optional preference text, 1–2,000 characters. |
| `preferences.target_cost_usd` | Positive soft spending target. |
| `preferences.target_duration_seconds` | Positive soft duration target. |
| `min_confidence` | Router confidence threshold, 0–1; default 0.75. |
| `low_confidence_fallback` | `"proposed"` (default) or `"frontier"`. |
| `frontier_model`, `frontier_thinking` | Preferred eligible fallback model and effort. |
| `min_success_rate`, `estimates` | Advanced matched-measurement constraints; absent evidence cannot satisfy a positive success threshold. |

Unknown policy fields fail validation. The shared schema also accepts `strategy`, `objective`, `weights`, and `oss_thinking`; standalone accepts only `strategy: "direct"`, which is the default. Use candidate IDs to restrict effort. See [the shared routing policy](https://github.com/gakonst/nanocodex/blob/master/docs/THREAD_ROUTING_PREFERENCES.md) for advanced measurement fields; its managed-agent and ChatGPT-specific features do not apply here.

For example, to restrict a session to one model/provider/effort:

```json
{"routing":{"candidates":["@cf/zai-org/glm-5.3:medium"]}}
```

Preferences are not probabilities, quotas, billing caps, or deadlines. Router confidence and provider latency do not establish task success rates. A route pin does not guarantee provider availability.

Once chosen, the `route` object reports `version`, `policy_version`, `backend`, `model`, `provider_model`, `thinking`, `reasoning_mode`, `fast_mode`, `family`, `confidence`, `objective`, `selection`, `created_at`, and `router_duration_ms`. Free-form routing reasons, raw router usage, and full audit payloads are omitted. The response body also includes this route.

New routing decisions also include an optional `route.diagnostics` allowlist. Older pinned sessions may omit it. `route.confidence` is the task-family classifier confidence; it is not the model-choice confidence.

| Diagnostic field | Meaning |
| --- | --- |
| `source`, `signal_kind` | `typesafe/jev`; these are choice probabilities and confidence, not task-success predictions. |
| `eligible_candidates` | Exact catalog candidate IDs considered for this request. |
| `proposed_candidate`, `chosen_candidate` | Jev's valid proposal, if available, and the final candidate after fallback policy. |
| `candidate_confidence`, `family_confidence` | Separate confidence scores in [0,1], or `null` when Jev was unavailable/invalid. |
| `candidate_probabilities` | Jev's probability for each eligible candidate, keyed by exact candidate ID; `null` if absent or invalid. |
| `family_probabilities` | Jev's probability for each diagnostic task family; `null` if absent or invalid. |
| `min_confidence`, `confidence_status` | Policy threshold and `accepted`, `low`, or `unavailable_or_invalid`. |
| `fallback_basis` | `none`, `valid_proposal`, or `eligible_frontier`. A low-confidence valid proposal may be retained. |

The two probability maps are independent distributions from [Jev's two choice questions](https://developers.cloudflare.com/ai/models/typesafe/jev/). They are included only when all expected choices have finite probabilities in [0,1] and the total is 1 within two-decimal rounding tolerance (0.005 per choice). Rounded displayed probabilities may therefore sum slightly above or below 1. The server does not fill in missing choices, renormalize a distribution, or derive probabilities from confidence. The projection excludes prompt text, free-form explanations, raw usage, and the full policy/audit payload.


## Generate with the optional session extension

```sh
curl --fail-with-body "$NANOCODEX_BASE_URL/responses" \
  -H "Authorization: Bearer $NANOCODEX_INFERENCE_KEY" \
  -H 'Content-Type: application/json' \
  --data "$(jq -n --arg session_id "$SESSION_ID" '{
    session_id:$session_id,
    model:"auto",
    stream:false,
    input:[{role:"user",content:"Explain why the sky is blue in two sentences."}],
    max_output_tokens:512
  }')"
```

A session response follows a bounded Responses-format contract. Expect `id`, `object: "response"`, `status`, `model`, `output`, `usage`, `session_id`, `route`, and `buffering: "buffered"`. Text is in message output items' `content` entries with `type: "output_text"`; there is no promised top-level `output_text` convenience field. Stateless responses have the same output format and route metadata, with no `session_id`. Output can also contain reasoning or tool-call items. Handle `status: "incomplete"` and `incomplete_details`, including output-token exhaustion.

Both JSON and buffered SSE responses expose the selected route in headers. Session ID headers apply only to the session extension; stateless responses do not create a reusable session:

| Header | Value |
| --- | --- |
| `x-nanocodex-session-id` | Public session UUID. |
| `x-nanocodex-provider` | Selected backend: `workers_ai`, `cloudflare`, `openrouter`, or `vercel`. |
| `x-nanocodex-model` | Canonical selected model. |
| `x-nanocodex-thinking` | Selected `low`, `medium`, or `high` effort. |
| `x-nanocodex-inference-session-id` | Same public session UUID. |
| `x-nanocodex-inference-buffering` | `buffered`. |
| `cache-control` | `no-store`. |

### Accepted request fields

| Field | Contract |
| --- | --- |
| `session_id` | Optional session UUID from `/v1/inference/sessions`; omission makes the request stateless. Sessions are owned by the exact inference key. |
| `model` | `"auto"` (default), an eligible canonical `model`, or exact candidate `id` from `/models`. With a session pin, an explicit model/candidate must match that pin. |
| `input` | Required nonempty string, or 1–1,024 history items. |
| `instructions` | Optional string; resend on every request when needed. |
| `stream` | Boolean; default `false`. See buffered SSE below. |
| `max_output_tokens` | Positive integer up to the key's limit and service ceiling of 4,096; omission uses the key limit. |
| `reasoning` | Optional `{ "effort": "low" \| "medium" \| "high" }`; filters selection; for a session, must match any retained route. |
| `tools` | Up to 128 function/custom definitions with unique names. |
| `tool_choice` | `"auto"`, `"none"`, `"required"`, or `{ "type": "function" \| "custom", "name": "..." }`. A named choice must match a supplied definition. |
| `parallel_tool_calls` | Optional boolean. |
| `temperature` | Number from 0 through 2. |
| `top_p` | Number from 0 through 1. |
| `text` | Only `{ "format": { "type": "text" } }`. |
| `store` | Only `false`, if supplied. |

This is a subset, not a drop-in implementation of every Responses API feature. Unknown fields fail validation. Unsupported features include `previous_response_id`, server-restored history, response retrieval, background jobs, opaque compaction, encrypted reasoning, image/audio/file input, built-in web/computer/MCP tools, and structured JSON output modes.

History supports:

- Messages with `role` of `user`, `assistant`, `system`, or `developer`, optional `type: "message"`, and string content or text parts (`input_text`, `output_text`, or `text`). Output annotations must be empty.
- `function_call` with `name`, `call_id`, and JSON-object arguments encoded as a string; its matching `function_call_output` carries the same `call_id` and string/text output.
- `custom_tool_call` with `name`, `call_id`, and string `input`; its matching `custom_tool_call_output` carries string/text output.
- Returned `reasoning` items containing plain `summary_text` or `reasoning_text` content.

Retain returned item IDs and status when replaying. Every historical tool call must have one matching output before the next user/assistant message. Duplicate call IDs, unmatched outputs, or pending calls at the end of a submitted history are rejected.

### Optional full-history session client: Python

Python 3, standard library only. This example makes one session and two response requests. It has no automatic retries.

```python
import json
import os
import urllib.error
import urllib.request

BASE = os.environ.get("NANOCODEX_INFERENCE_BASE",
                      "https://nanocodex.gakonst.workers.dev/v1/inference")
KEY = os.environ["NANOCODEX_INFERENCE_KEY"]

def api(path, body=None, method=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(
        BASE + path, data=data, method=method or ("POST" if body is not None else "GET"),
        headers={"Authorization": "Bearer " + KEY, "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=150) as res:
            return None if res.status == 204 else json.load(res)
    except urllib.error.HTTPError as err:
        raise RuntimeError(f"HTTP {err.code}: {err.read().decode()}") from err

def response_text(response):
    return "".join(part["text"]
                   for item in response["output"] if item["type"] == "message"
                   for part in item["content"] if part["type"] == "output_text")

session = api("/sessions", {"routing": {"preferences": {"cost": 70}}})
history = []
for prompt in ["Explain rainbows briefly.", "Now explain it to a child."]:
    history.append({"role": "user", "content": prompt})
    response = api("/responses", {
        "session_id": session["id"], "model": "auto", "stream": False,
        "input": history, "max_output_tokens": 512,
    })
    history.extend(response["output"])
    print(response_text(response))
    print("Route:", response["route"])
    if response["status"] != "completed":
        raise RuntimeError("Incomplete response; inspect incomplete_details before continuing")

# Only after saving any history your application needs:
api("/sessions/" + session["id"], method="DELETE")
```

### Optional full-history session client: Node.js

Node.js with built-in `fetch`. Save as an `.mjs` file. This example also uses no automatic retries.

```js
const base = process.env.NANOCODEX_INFERENCE_BASE
  ?? "https://nanocodex.gakonst.workers.dev/v1/inference";
const key = process.env.NANOCODEX_INFERENCE_KEY;
if (!key) throw new Error("Set NANOCODEX_INFERENCE_KEY");

async function api(path, body, method = body === undefined ? "GET" : "POST") {
  const response = await fetch(base + path, {
    method,
    headers: { Authorization: `Bearer ${key}`, "Content-Type": "application/json" },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    signal: AbortSignal.timeout(150_000),
  });
  if (!response.ok) throw new Error(`HTTP ${response.status}: ${await response.text()}`);
  return response.status === 204 ? undefined : response.json();
}

const session = await api("/sessions", { routing: { preferences: { cost: 70 } } });
const history = [];
for (const prompt of ["Explain rainbows briefly.", "Now explain it to a child."]) {
  history.push({ role: "user", content: prompt });
  const response = await api("/responses", {
    session_id: session.id, model: "auto", stream: false,
    input: history, max_output_tokens: 512,
  });
  history.push(...response.output);
  console.log(response.output.filter(item => item.type === "message")
    .flatMap(item => item.content).filter(part => part.type === "output_text")
    .map(part => part.text).join(""));
  console.log("Route:", response.route);
  if (response.status !== "completed") {
    throw new Error("Incomplete response; inspect incomplete_details before continuing");
  }
}
await api(`/sessions/${session.id}`, undefined, "DELETE");
```

## Client-side function tools

Example definition:

```json
{
  "type": "function",
  "name": "lookup_temperature",
  "description": "Get the current temperature for a city.",
  "parameters": {
    "type": "object",
    "properties": { "city": { "type": "string" } },
    "required": ["city"],
    "additionalProperties": false
  }
}
```

A returned `function_call` includes `name`, `call_id`, and a JSON string in `arguments`. Your application validates the name and arguments, decides whether execution is authorized, performs its own operation, and appends a result such as:

```json
{"type":"function_call_output","call_id":"RETURNED_CALL_ID","output":"{\"temperature_c\":21}"}
```

Resend the entire preceding history, including the returned call and matching output, for the next inference. Resend tool definitions on each request where the model may call them. The service does not execute the function, fetch URLs in its arguments, access an account connector, or authorize an external action.

Function `strict: true` is unsupported; omit it or use `false`/`null`. Your application must validate schema conformance. Custom tools accept text or a `lark`/`regex` grammar description, but that grammar is supplied as instructions and is not enforced by the provider.

## Streaming is buffered SSE

With `stream: true`, the response is `text/event-stream`. **Generation finishes upstream before SSE is returned.** Delta events are a buffered rendering of the completed result, not live token delivery. Both JSON and SSE responses carry `x-nanocodex-inference-buffering: buffered`; response objects carry `buffering: "buffered"`.

The event sequence uses Responses-style `response.created`, output-item/content-part events, text/tool-argument deltas and done events, and `response.completed` or `response.incomplete`. Parse the terminal event's `response` for output and usage. Do not depend on a `[DONE]` sentinel. Errors before successful generation use ordinary HTTP error responses rather than successful SSE.

The inference timeout is 120 seconds. Client cancellation stops waiting; Workers AI's binding does not expose cancellation of already-running provider inference. Do not interpret an interrupted connection as proof that generation did not run.

## Limits and retries

Default inference-key limits are **100 reserved POST requests per UTC day, 10 per UTC minute, and 4,096 output tokens per inference**, with expiry 30 days after issuance. Limits can be lower for a particular key; check metadata supplied by the issuer. Daily/minute windows are fixed UTC windows, not sliding windows. HTTP 429 includes a `Retry-After` value in seconds.

Quota is reserved before body validation for both `POST /v1/inference/sessions` and response POST requests on either alias. A rejected body or later provider failure can therefore still consume a slot. Authentication failures and quota-rejected attempts do not reserve an additional slot. Model listing and session GET/DELETE authenticate the key without reserving generation quota. Session counters describe admitted generation attempts, not the key's complete quota usage.

Additional ceilings:

| Limit | Value |
| --- | --- |
| Public session/response request body | 262,144 bytes |
| JSON-encoded `input` plus JSON-encoded `instructions` | 32,768 bytes |
| History items | 1,024 |
| Tool definitions | 128 |
| Output tokens | At most 4,096 and no more than the key cap |
| Concurrent requests to one session | One; conflicts return HTTP 409 `session_busy` |
| Inference timeout | 120 seconds |
| Admin issuance request body | 4,096 bytes |

Session creation and generation have **no idempotency-key or response-replay guarantee**. Retrying `POST /v1/inference/sessions` creates another session. Retrying a response POST can run another generation and consume another quota slot. Only the optional session extension retains its route across requests; stateless auto requests select afresh. Neither mode promises identical output. No endpoint recovers a lost generated response. Do not blindly retry after a network timeout or uncertain result.

To handle `session_busy`, wait for the outstanding request to finish. To handle quota exhaustion, respect `Retry-After`. Key issuance has a separate `operation_id` mechanism described below; it does not apply to inference requests.

## Errors

The standard `/v1/responses` and `/v1/models` aliases return HTTP errors in the SDK-compatible envelope below, preserving the HTTP status and headers such as `Retry-After`:

```json
{"error":{"message":"unauthorized","type":"authentication_error","param":null,"code":"unauthorized"}}
```

`type` is `authentication_error` for HTTP 401, `rate_limit_error` for 429, `server_error` for 5xx, and `invalid_request_error` for other errors. Use `code` for programmatic handling. The `/v1/inference/*` extension paths retain their earlier mixed shapes: `{"error":"CODE"}` or `{"error":{"code":"CODE"}}`; clients using those paths must normalize both. Provider error bodies, deployment secrets, and internal exception messages are not exposed.

| HTTP | Representative codes | Meaning |
| --- | --- | --- |
| 400 | `invalid_request`, `invalid_inference_request`, `invalid_json`, `session_id_required` | Malformed body or session ID, unknown or unsupported fields. |
| 400 | `invalid_routing_policy`, `unsupported_routing_strategy`, `unknown_model`, `no_inference_candidates` | Invalid routing policy/model or no eligible candidate matching the model and effort. |
| 400 | `invalid_tool_history`, `duplicate_tool_name`, `invalid_tool_choice` | Invalid tool definitions or full-history pairing. |
| 400 | `max_output_tokens_exceeds_key_limit` | Requested output exceeds this key's cap. |
| 401 | `unauthorized` | Missing, invalid, expired, or revoked inference key; also missing admin authority. |
| 403 | `inference_key_scope`, `inference_key_admin_required`, `forbidden`, `forbidden_origin` | Wrong credential scope, account is not the configured deployment operator, insufficient admin capability, or failed browser origin check. |
| 404 | `not_found`, `session_not_found` | Unknown route/session/key, deleted session, or session owned by another key. |
| 405 | `method_not_allowed` | Unsupported HTTP method. |
| 409 | `session_busy`, `route_is_pinned` | Concurrent session operation or attempt to change a pinned model, candidate, or reasoning effort. |
| 409 | `already_issued` | An issuance operation ID was already claimed; metadata only is returned. |
| 413 | `body_too_large`, `request_too_large`, `input_too_large`, `payload_too_large` | Request/input exceeds its size ceiling. |
| 415 | `expected_json` | Key issuance requires `Content-Type: application/json`. |
| 429 | `rate_limit_exceeded` | Per-key minute/day quota exceeded. |
| 499 | `request_cancelled` | The public request was cancelled while forwarding. |
| 502 | `inference_failed`, `inference_unavailable` | Provider, translation, routing, or private forwarding failed; route may already be retained. |
| 503 | `inference_unavailable`, `inference_auth_unavailable`, `inference_key_write_unavailable`, `invalid_pinned_route` | Service configuration/storage unavailable or invalid route state. |
| 504 | `inference_timeout` | Server deadline elapsed; do not assume upstream cancellation. |

Inspect session metadata after a failed first response to determine whether routing was pinned. Deleting a session returns 204; subsequent operations on it return 404. Deletion removes access to that session and retains a tombstone; it does not revoke the inference key.

## Administrator: issue, list, and revoke keys

These operations require the **configured deployment operator account**, authenticated by a full-account API key or browser session, with the corresponding `api_keys:read`/`api_keys:write` capability. The authenticated account must match `NANOCODEX_ADMIN_USER_ID`; other accounts receive HTTP 403 `inference_key_admin_required`, even if they hold key-management capabilities. An unset operator ID also denies administration. This restriction protects deployment-funded credits; ordinary teammates receive inference keys and use only the data endpoints. Connect grants and inference keys cannot administer keys. Browser-session mutations require an exact same-origin `Origin` header. The following examples use a server-side full-account key:

```sh
export NANOCODEX_ACCOUNT_API_KEY='YOUR_FULL_ACCOUNT_API_KEY'
```

Issue **one** key with an explicit UUID `operation_id`. Use a fresh UUID for each intentionally distinct key. Times use Unix milliseconds.

```sh
EXPIRES_AT=$(node -e 'console.log(Date.now() + 30 * 24 * 60 * 60 * 1000)')
OPERATION_ID=$(node -e 'console.log(crypto.randomUUID())')
curl --fail-with-body "$NANOCODEX_INFERENCE_BASE/keys" \
  -H "Authorization: Bearer $NANOCODEX_ACCOUNT_API_KEY" \
  -H 'Content-Type: application/json' \
  --data "$(jq -n --arg operation_id "$OPERATION_ID" --argjson expires_at "$EXPIRES_AT" '{
    operation_id:$operation_id,
    label:"trial-01",
    expires_at:$expires_at,
    limits:{requestsPerDay:100,requestsPerMinute:10,maxOutputTokens:4096}
  }')"
```

The response is `{"api_key":"ONE_TIME_BEARER_TOKEN","key":{...metadata...}}`. Store the token securely when issued; listing and retries never return it. Metadata contains `id`, `label`, `scope: "inference"`, `createdAt`, `expiresAt`, `limits`, and `revokedAt`; timestamps are Unix milliseconds; `revokedAt` is `null` until revoked. Issued keys always have an expiry. Digests and provider credentials are never returned.

Scope is assigned by the server and cannot be supplied or changed by the caller. Accepted creation fields are `label` (default `"Inference key"`, at most 120 characters), `expires_at` (future Unix milliseconds; `null` is rejected), `limits` (partial overrides using the camelCase names above), and `operation_id` (UUID). **Omitting `expires_at` defaults to 30 days after issuance.** The explicit timestamp above makes that trial period reviewable. Limits accept positive integers up to 1,000,000 requests/day, 10,000 requests/minute, and 4,096 output tokens. Ten trial keys means ten separately authorized issuance calls, each with a distinct operation ID and label; there is no batch-count field. This guide does not assert that any keys have been issued.

Issuance claims `operation_id` once, before storing the key. Repeating a claimed ID returns HTTP 409 `already_issued` with metadata, never another token. After an uncertain issuance result, reuse the same ID to reconcile and list metadata; do not automatically create another key with a new ID. A claimed operation can survive a later storage failure. Revoke its metadata entry before intentionally issuing a replacement when the original bearer was lost or issuance could not be confirmed.

List metadata:

```sh
curl --fail-with-body "$NANOCODEX_INFERENCE_BASE/keys" \
  -H "Authorization: Bearer $NANOCODEX_ACCOUNT_API_KEY"
```

Revoke by metadata `id`, not by bearer value:

```sh
curl --fail-with-body -X DELETE "$NANOCODEX_INFERENCE_BASE/keys/YOUR_KEY_ID" \
  -H "Authorization: Bearer $NANOCODEX_ACCOUNT_API_KEY"
```

Revocation returns HTTP 204 and is checked on subsequent inference API calls. Already-admitted generation is not forcibly cancelled. A replacement key does not inherit the revoked key's sessions. Repeated revocation of an owned key remains successful; an unowned/unknown key returns 404. The metadata listing retains revocation state.

## Source and isolation boundaries

The API authenticates through dedicated key Durable Objects and executes stateless requests without creating a persistent session. The optional extension uses dedicated session Durable Objects. Neither path constructs a managed agent runtime. The gateway replaces internal key/session headers after authentication. Sessions are bound to key IDs, tools are returned without execution, provider destinations are fixed by the server, and the stored session contains routing metadata/counters rather than conversation transcripts. The full supplied history still goes to the selected inference provider; this is not a provider retention guarantee.

Implementation references:

- `js/managed/src/inference-api.ts`: public paths, scope guard, body bound, key quota reservation, model catalog, private forwarding.
- `js/managed/src/inference-keys.ts`: key digest validation, atomic quotas, metadata registry, expiry, issuance claims, revocation.
- `js/managed/src/inference-session.ts`: strict request subset, full-history validation, route pin, generation, response formats, session ownership.
- `js/managed/src/account-auth.ts` and `js/managed/src/index.ts`: account-auth and early route integration boundaries.
- `js/nanocodex/cloudflare/gateway-responses.mjs` and `workers-ai-responses.mjs`: stateless provider adapters and buffered SSE.

Publish copies of this guide with placeholders only. Deployment receipts, real key values, account identifiers, and private operational output do not belong in a shared guide or Gist.
