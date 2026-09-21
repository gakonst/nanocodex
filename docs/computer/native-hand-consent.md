# Native Hand computer access

Nanocodex delegates computer use to the installed official OpenAI CUA provider.
The native Hand and desktop app forward the provider's tool declarations, calls,
and results. Application policy, operating-system permissions, and any approval
UI supplied by the official provider remain upstream responsibilities.

Nanocodex does not add a terminal, AppKit, or WinForms approval dialog, remember
application consent, or manufacture approval responses. Its MCP client does not
advertise form elicitation. Unsupported provider-to-host requests, including
`elicitation/create` and `openai/elicitation/create`, receive a JSON-RPC
method-not-found error (`-32601`); they are never automatically accepted.

An upstream operation that requires a host approval mechanism unavailable through
this transport can fail. Removing Nanocodex's custom handlers does not establish
that every upstream operation supports this client. Use the official provider's
supported permission flow when required; do not substitute a tool argument or
synthetic response for user consent.
