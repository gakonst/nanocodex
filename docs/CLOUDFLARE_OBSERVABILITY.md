# Cloudflare Workers observability

Nanocodex uses Cloudflare's native Workers Logs and automatic traces for the
production Worker topology. Safe internal Workers and static surfaces retain
invocation logs and traces at a 100% head sampling rate; there is no application
telemetry backend or external export destination.

The public root Worker and Connect API are deliberate exceptions: they receive
OAuth callback query parameters and one-use WebSocket tickets. Cloudflare's
platform-generated invocation logs and automatic trace attributes include the
request URL, so those two Workers disable persistent observability. Use bounded
real-time tailing only during a controlled reproduction, then investigate the
sanitized correlated events emitted by the managed-agent and egress Workers.

## Operator access

Open the account-level [Workers Observability dashboard](https://dash.cloudflare.com/?to=/:account/workers-and-pages/observability/).
**Overview** spans all Workers in the account. Use **Investigate** for the query
language and **Invocations** to group the events and trace for one invocation.
Cloudflare controls the retained-data window for the account plan.

For agent-assisted, read-only investigation, connect Cloudflare's official
Workers Observability MCP server and complete its Cloudflare authorization:

```text
https://observability.mcp.cloudflare.com/mcp
```

Do not paste API tokens, session credentials, or production data into the MCP
configuration or prompts.

## Stable structured fields

Application operational events should use these top-level fields when the
corresponding identity exists:

| Field | Meaning |
| --- | --- |
| `user_id` | Opaque Nanocodex account ID |
| `organization_id` | Opaque organization ID |
| `team_id` | Opaque team ID; do not derive it from `organization_id` |
| `agent_id` | Opaque hosted-agent ID |
| `thread_id` | Opaque conversation/thread ID |
| `turn_id` | Opaque turn ID |
| `grant_id` | Opaque Connect grant ID, never the grant credential |
| `connector` | Bounded connector name, not provider configuration |
| `deployment_sha` | Exact 40-character deployed Git revision |
| `type` | Stable dotted event name, such as `connect.grant.create` |
| `outcome` | Bounded result such as `success`, `failure`, or `cancelled` |
| `status` | Bounded string state or safe error code, never raw error text |

Keep each field's type and meaning stable. Omit unavailable fields instead of
using guessed, empty, or repurposed values.

## Safe logging boundary

Logs and trace attributes contain operational metadata only. Never record
prompts, replies, tool arguments or results, memory, imported content, request
or response bodies, raw provider errors, or URLs containing query strings.
Never record Authorization or Cookie values, passkey material, app/grant bearer
credentials, provider credentials, connector tokens, private keys, or secret
configuration. Emit opaque IDs, bounded enums/codes, counts, sizes, and timings;
map failures to a safe code before logging.

Do not enable persistent logs or automatic traces on a Worker that terminates a
credential-bearing callback unless the callback is first moved behind a native
boundary that cannot retain its original URL.

## Native queries

Paste each query into the **Investigate** search bar and replace the quoted
placeholder with the exact opaque ID or revision.

User:

```text
user_id = "usr_01JEXAMPLE"
```

Team:

```text
team_id = "team_01JEXAMPLE"
```

Agent:

```text
agent_id = "agent_01JEXAMPLE"
```

Application failures plus uncaught Worker exceptions:

```text
outcome = "failure" OR status = "internal_error" OR $metadata.error EXISTS OR $workers.outcome = "exception"
```

Deployment:

```text
deployment_sha = "0123456789abcdef0123456789abcdef01234567"
```

Add `AND deployment_sha = "..."` or another identity field to narrow any query.
Cloudflare source maps make exception stacks readable in the dashboard without
publishing source maps to application clients.
