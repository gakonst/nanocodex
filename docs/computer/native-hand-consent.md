# Native Hand consent

A foreground `nanocodex2 hand` on macOS or Linux can present CUA MCP form
elicitation in its controlling terminal, including a dedicated tmux pane. Run
that Hand in the foreground and leave the pane available to the person operating
it. The handler is installed before external provider discovery, so the MCP
initialize request advertises form elicitation only when a real foreground
canonical terminal is available on stdin. The handler opens a separate descriptor
for the resolved terminal device; it does not read stdin. This avoids macOS
kqueue rejecting the `/dev/tty` alias. The existing provider configuration and sandbox
are unchanged.

When Sky asks for application access, the terminal shows the complete provider
form, including its `_meta`, and the originating session/call when available.
Provider text is displayed as escaped JSON. The person types one of these lines,
for the currently displayed request:

```
accept
decline
cancel
```

For a form with fields, append the JSON object matching its displayed
`requestedSchema`. Acceptance is sent only after explicit input after the current prompt appears and valid form content.
Old terminal input is discarded before displaying a new request. Qualified
responses containing the request ID remain supported. No field defaults are added. A plain `accept` never adds persistence metadata. Decline and cancel send no content. Malformed or stale answers leave the
form pending. When the provider offers session persistence and supplies a connector/tool/app
scope, the terminal also offers `accept-session`. Only this
explicit choice returns `_meta.persist = "session"` and remembers the response.
Reuse requires the same live provider process, conversation, and complete form
parameters except the call correlation fields `_meta.progressToken` and
`_meta.tool_call_id`, plus transport `_meta.x-codex-turn-metadata`. The connector, tool name, exact tool
parameters, displayed scope, schema, and risk metadata must all remain identical.
Different operations can therefore require separate decisions. Reset/restart
expires the permission. No `always` permission is offered or stored on disk.

Responses go directly to the CUA provider and are not logged as
model/tool input.

The terminal is read with cancellable, nonblocking I/O. Provider cancellation,
call completion, or host timeout drops the pending review and invalidates its
request ID. EOF cancels. Forms are serialized across conversations within the
Hand. The existing `ComputerConfig.elicitation_timeout` remains the overall
response deadline (five minutes by default); an enclosing tool deadline can
cancel sooner. Oversized forms and unsupported schemas fail closed.

This path is for a local person operating a dedicated foreground Hand terminal.
The agent must never enter consent responses through shell, tmux, or CUA tools.
It does not provide a remote approval inbox. Detached services without a
controlling terminal, background process groups, raw-mode TUIs, and Windows
advertise no local terminal elicitation handler. Native CLI/TUI agent sessions
are intentionally not wired to this reader because their event loop owns input.

Managed UI consent requires a wider protocol change: the attachment transport
currently carries tool calls/results, cancellation, heartbeats, and draining,
but has no provider-to-client form request or authenticated user-response frame.
The existing Vault review is tied to Vault operations and cannot supply a generic
CUA decision. A managed form implementation must bind replies to the requesting
Hand/session/call, dismiss on cancellation, and carry an authenticated user
response rather than a model tool argument.
