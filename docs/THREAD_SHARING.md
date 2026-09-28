# Sharing a managed thread

A signed-in thread owner can create a **view** link or a **comment** link from the
website's Share control. Anyone holding the link can open its guest view without
signing in. A view link shows the thread's user and assistant messages; a comment
link also lets its holder add an attributed comment. Guest comments are not
agent prompts: opening a link never gives the visitor the owner's model, tools,
connected accounts, memory, workspace, or control of an active turn.

Treat a link as a secret. It grants access to potentially sensitive conversation
text until revoked. The secret is carried in the URL fragment (`#token=…`),
which browsers do not send over HTTP. The guest view sends the token only in an
`Authorization: Bearer` header to the exact shared-thread API. The server stores a digest, not
the plaintext token. The creation response is the only time the full link is
available; listing existing links returns metadata but not their secrets.
Revoke a link from the Share control to stop new reads and comments. A person
who already saw or copied conversation text may retain their own copy.

## Terminal UI

In a hosted `nanocodex2` thread, enter `/share` for help, `/share read` to
create a view link, or `/share write` to create a comment link. The new link
is copied to the clipboard when available and displayed once in an owner-only
panel; treat it as a bearer credential. `/share list` shows link IDs and
permissions **without** their URLs. `/share revoke <link-id>` disables one.
After an uncertain network error, check the list before repeating a mutation.
Local-only `nanocodex` threads have no hosted URL and cannot be shared this way.

## HTTP contract

| Method | Path | Authority | Result |
| --- | --- | --- | --- |
| `GET` | `/v1/agents/:id/share-links` | Thread owner | Active link metadata, no secrets |
| `POST` | `/v1/agents/:id/share-links` | Thread owner | Create a link with `{ "permission": "read" }` or `{ "permission": "write" }`; returns its URL once |
| `DELETE` | `/v1/agents/:id/share-links/:linkId` | Thread owner | Revoke this link |
| `GET` | `/v1/agents/:id/share-comments` | Thread owner | Guest comments without a bearer link |
| `GET` | `/v1/shared/:id` | Link bearer | Guest thread metadata and permission |
| `GET` | `/v1/shared/:id/events/history` | Link bearer | Paginated user/assistant transcript projection, not raw tool events |
| `GET` | `/v1/shared/:id/comments` | Link bearer | Guest comments |
| `POST` | `/v1/shared/:id/comments` | Comment-link bearer | Add `{ "id": "unique-comment-id", "input": "text" }` |

A signed-in account session or account API key with agent read/write capability can
administer links for its own threads. Delegated Connect grants and guest tokens
cannot administer them. A guest token is scoped to one
thread and these specific endpoints. It cannot be used on normal agent,
configuration, files, attachments, tool, or turn-control routes. Browser writes
require a same-origin request. The shared transcript deliberately excludes tool
calls, tool output, reasoning, and other non-conversation events. History and
comments are paginated; follow `next_cursor` while `has_more` is true, even
when a history page has no visible messages after filtering. These restrictions
are enforced by the managed service, not by hiding UI controls.
