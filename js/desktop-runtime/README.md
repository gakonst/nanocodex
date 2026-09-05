# Nanocodex desktop runtime

`@nanocodex/desktop-runtime` owns the managed connection, durable thread event
observers, preferences, and compute Hand lifecycle shared by the Electron and
Swift applications. Apps own their native UI, file pickers, OS credential store,
and packaging. Neither app imports another app's source.

`pnpm --filter @nanocodex/desktop-runtime build` bundles `dist/host.mjs` and its
lazy chunks for a Node 22.13+ host. Copy the complete `dist` directory when
packaging the native app. `pnpm --filter @nanocodex/desktop-runtime test` covers
the transport, scope, persistence, startup, and shutdown boundaries.

The host reads one JSON request per stdin line:

```json
{"id":1,"method":"state","args":[]}
{"id":2,"method":"openThread","args":["agent-id"]}
```

Each request gets exactly one `{ "id": ..., "result": ... }` or
`{ "id": ..., "error": "message" }` response while the host remains open.
Asynchronous state is `{ "event": { "type": "state", "state": ... } }` or
`{ "event": { "type": "thread", "thread": ... } }`. Response IDs allow actions
and event streams to proceed concurrently. Undefined results are JSON `null`.
An unknown action, duplicate pending ID, malformed arguments, or oversized
request receives an error. Stdout is reserved for this protocol.

The allowlist is `state`, `connect`, `disconnect`, `refresh`, `openThread`,
`closeThread`, `older`, `createThread`, `prompt`, `steer`, `cancel`, `settings`,
`saveLayout`, `saveHand`, `prepareFolderHand`, `startHand`, `stopHand`, and
`removeHand`. There is no
arbitrary fetch, command, filesystem, or subprocess bridge.

`NANOCODEX_DESKTOP_DATA` chooses the app's private state directory.
`NANOCODEX_ENV_FILE` optionally supplies development configuration; normal
environment values take precedence. The managed credential is `NC_API_KEY` or
`NANOCODEX_API_KEY`, with an optional `NANOCODEX_MANAGED_URL`. The host does not
write credentials to disk. The native app owns Keychain and may instead call
`connect` after launch. Only drafts, tab preferences, and stopped Hand
configurations are retained, fenced by a digest of the origin and credential.
Credentials are never included in state or events.

`state.hasCredentials` distinguishes sign-in from a pending connection.
Authenticated requests have a 20-second deadline; SSE uses the managed SDK's
inactivity/reconnect behavior. Thread events are batched over 32 milliseconds,
deduplicated by durable cursor, and replayed after reconnect. Closing a tab stops
its observer without canceling its managed agent. Stdin EOF or SIGTERM closes
every observer and Hand before exiting.

A local Hand runs real native commands in an explicitly selected workspace.
The workspace is a working directory, not an OS sandbox. Native process tools
support retained pipe sessions and do not inherit API credentials. VM Hands use
the existing `nanocodex2 hand` CLI and Linux guest runtime. A selected base image
is cloned to an account-scoped private writable disk; its source is untouched.
The VM's cache also lives in the app's private directory. A VM is connected only
after the CLI emits `vm.hand.ready`. Stopping during startup cancels setup and
closes any partially acquired resources.

`prepareFolderHand({ agentId, workspace })` connects a folder when the user sends
a message in that folder's tab. It reuses an existing eligible Hand or creates a
thread-scoped one, chooses its name and ID, starts it, and returns the connected
Hand. Merely selecting a folder does not start compute. Concurrent retries are
deduplicated, and account changes fence the operation.

On macOS, an imported native helper must carry a valid hypervisor entitlement.
If it does not, the runtime clones the helper into its own cache, ad-hoc signs
that private copy, and verifies its signature and entitlement before launch.
The selected executable is never modified. An existing correctly signed release
helper runs directly.

Optional VM defaults are `NANOCODEX_HAND_BINARY`, `NANOCODEX_VM_ROOTFS`, and
`NANOCODEX_VM_GUEST_RUNTIME`. Development hosts can also discover the built CLI
and guest ELF beside `NANOCODEX_ENV_FILE`. Cloud Hands and inventory discovery
remain ordinary managed agent tool calls through the canonical service; the
runtime does not invent a separate cloud provisioning API.
