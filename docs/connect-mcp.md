# Nanocodex MCP

Nanocodex exposes a remote MCP server at `https://nanocodex.gakonst.workers.dev/mcp`.
External clients sign in through Nanocodex Connect and receive a scoped OAuth
credential. The endpoint requires an account deployment containing this feature.

## Codex

```sh
codex mcp add nanocodex --url https://nanocodex.gakonst.workers.dev/mcp
codex mcp login nanocodex
```

Review the requested access in the browser and approve it in Nanocodex. Use
`codex mcp list` or `/mcp` to inspect the connection. The CLI and IDE extension
share MCP configuration. See the [official Codex MCP guide](https://developers.openai.com/codex/mcp).

## Claude Code

```sh
claude mcp add --transport http nanocodex https://nanocodex.gakonst.workers.dev/mcp
```

Open `/mcp` in Claude Code and authenticate the Nanocodex server. Review and
approve access in the browser, then return to Claude Code. See the
[official Claude Code MCP guide](https://code.claude.com/docs/en/mcp).

## Claude and other clients

In Claude's connector settings, add a custom remote connector with the same MCP
URL and complete OAuth. Organization policies may require an administrator to
add it first. Other clients need Streamable HTTP and authorization-code OAuth
with PKCE S256 and dynamic client registration. No manually copied account API
key or OAuth client secret is required.

The approval page shows the requesting client's name, callback destination,
account, and requested permissions. Client names are supplied by clients;
check the destination before approving.

## Tools and permissions

The server lists tools permitted by the current OAuth scopes and live Connect
grant. The approval screen lets you select a subset of the requested scopes.
Select the services or data access you want to share before approving.
An existing connection does not gain new permissions automatically; reconnect
and approve the additional access.

| OAuth scope | MCP tools |
| --- | --- |
| Any approved connection | `nanocodex_connection` |
| `agent:run` | `nanocodex_agent_start`, `nanocodex_agent_status` |
| `data:read`, `data:write` | `nanocodex_data_read`, `nanocodex_data_write` |
| `memory:read`, `memory:write` | `nanocodex_memory_read`, `nanocodex_memory_write` |
| `history:read` | `nanocodex_history_search`, `nanocodex_history_read` |
| `connector:<provider>` | `nanocodex_<provider>_request` for an approved connected provider |

Agent execution uses the approved ChatGPT connection. Direct connector, data,
and memory access can be authorized without agent execution. Connector calls
retain Connect's selected account identities, provider paths, and live grant
checks. `nanocodex_connection` identifies the approved connections without
returning credentials. Mutations still require the user's authorization.

Memory and history follow Connect's existing team access rules. This interface
does not turn a Connect token into unrestricted private account access. Account
API keys, OAuth MCP tokens, and internal Connect grant credentials are distinct.

Agent starts require a stable `operation_id`. Preserve it and the original prompt
after an uncertain result. An acceptance receipt does not mean the turn has
finished; inspect its status and final output. Do not automatically retry
connector writes after an uncertain response.

For a memory file mutation, use `nanocodex_memory_write` with
`operation: "write"` and `write_operation: "put"`, `"append"`, or `"delete"`.
The separate field distinguishes the memory API method from its file mutation.
Shared writes require explicit user direction. Data operations use their normal
`operation`, such as `document_get` or `document_put`; connector request bodies
may be JSON values or raw strings.

## OAuth and transport

Discovery uses RFC 9728 protected-resource metadata and RFC 8414 authorization
server metadata. Public clients register redirect URIs dynamically and use an
S256 PKCE authorization-code flow. Authorization binds the selected client,
redirect, requested scopes, and exact `/mcp` resource. Tokens are sent only in
the `Authorization` header. Refresh tokens rotate; reuse invalidates the token
family. Revocation and expiry are checked again before tool execution.

The transport supports legacy MCP revisions `2025-03-26`, `2025-06-18`, and
`2025-11-25`, plus the MCP2 draft revision `2026-07-28`, with JSON responses
over Streamable HTTP. Legacy clients use `initialize`; MCP2 clients use
`server/discover` and send matching protocol metadata and method headers on
every request. It is stateless and does not issue MCP session IDs or offer a
standalone GET event stream. Dynamic client registration is supported;
client-ID metadata document registration is not advertised.

MCP2 clients with approved `agent:run` authority can list and subscribe to
`agent.turn.completed` events through signed HTTPS webhooks. Durable alarms
observe MCP-started turns after the client disconnects. Subscriptions expire,
can be refreshed with rotating signing keys, and stop when their OAuth family
or Connect grant is revoked. Receivers must verify signatures and deduplicate
stable event IDs. See the [MCP Events API and callback requirements](../js/connect-api/README.md#mcp-events)
for request shapes, verification, delivery responses and transport restrictions.

## Operations

The account Worker forwards `/mcp`, OAuth endpoints under `/oauth/`, and OAuth
metadata to the Connect API Worker. `/connect-dialog` serves the approval UI.
Deploy the Connect API and Connect dialog before the account Worker using the
existing root deployment scripts. Use the same public origin throughout
metadata discovery, authorization, token exchange, and MCP requests.

Useful public checks:

```sh
curl -i https://nanocodex.gakonst.workers.dev/mcp
curl https://nanocodex.gakonst.workers.dev/.well-known/oauth-protected-resource/mcp
curl https://nanocodex.gakonst.workers.dev/.well-known/oauth-authorization-server
```

An unauthenticated MCP request returns HTTP 401 with OAuth discovery information.
OAuth codes, access tokens, refresh tokens, and approval links are sensitive;
keep them out of issue reports and logs.
