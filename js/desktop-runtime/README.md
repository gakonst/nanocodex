# Nanocodex desktop runtime

`@nanocodex/desktop-runtime` owns the managed connection, durable thread event
observers, preferences, and compute Hand lifecycle for the native tiling app in
[`macos/`](../../macos/README.md). The app owns its native UI, file pickers,
OS credential store, and packaging. This package is its runtime, not a separate
desktop app.

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
`closeThread`, `older`, `createThread`, `prompt`, `queuePrompt`, `steer`, `cancel`, `settings`,
`saveLayout`, `saveHand`, `prepareDefaultHand`, `prepareFolderHand`, `startHand`, `stopHand`, and
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

A local Hand runs real native commands in the default or selected workspace.
The workspace is a working directory, not an OS sandbox. Native process tools
support retained pipe sessions and do not inherit API credentials. VM Hands use
the existing `nanocodex2 hand` CLI and Linux guest runtime. A selected base image
is cloned to an account-scoped private writable disk; its source is untouched.
The VM's cache also lives in the app's private directory. A VM is connected only
after the CLI emits `vm.hand.ready`. Stopping during startup cancels setup and
closes any partially acquired resources.

`prepareDefaultHand()` reuses the account-wide local Hand, or creates one with
the default name and workspace, and returns it connected. The native app calls
this automatically after sign-in and restoration, retrying transient failures.
Concurrent calls share preparation; account changes invalidate it. Generic
runtime reads do not start compute.

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
and guest ELF beside `NANOCODEX_ENV_FILE`. Cloud provisioning remains an ordinary
managed agent tool call through the canonical service.

A configured account-wide local Hand exposes `list_vms`, `start_vm`, and
`stop_vm` to the account's agents, including conversations started on a phone.
For example, ask: **On my Mac Hand, start a VM named phone-demo, then run uname
inside that VM.** The caller supplies a stable name; the host owns the executable,
image, firmware, and credentials. `start_vm` returns the connected machine ID.
Repeating a name reconnects its retained disk. At most four VMs run concurrently;
stopping the parent Hand or quitting the app stops its VMs while preserving their
files. A failed launch returns its error, with a 90-second readiness deadline.
These VM Hands expose shell/files/processes; a graphical desktop requires a
desktop-enabled image and publisher.

Installed apps can retain a prepared recipe in `vm.json` under
`NANOCODEX_DESKTOP_DATA` (the Mac app uses
`~/Library/Application Support/Nanocodex/Native`). Its `binary`, `rootfs`, and
`guestRuntime` fields are absolute paths; optional `firmware` is the libkrunfw
directory. Environment settings override this file. Restart the Hand after
changing the recipe so its tool catalog refreshes. Assets must already be
prepared using the existing VM build tooling; this file contains no secrets.

Local VM hosting supports Apple Silicon macOS and glibc Linux with readable,
writable `/dev/kvm`. Linux containers need that device passed through from a
host with virtualization support. Native Windows VM hosting is not implemented;
a Windows machine would need a separately configured Linux/WSL2 host exposing
working nested KVM to use the Linux CLI. Windows and Intel Mac desktops do not
advertise these VM tools.

`refreshAccountHands()` reads the account-owned `/v1/account/hands` catalog.
The native app polls it every five seconds while connected. These devices are
separate from locally managed `hands`: they can be selected without creating or
starting a local process. Observed devices are cached in account-scoped
preferences and retained offline when absent or discovery fails. Cached entries
start offline after restart; only a fresh catalog marks them connected. A new
profile must observe a device connected at least once before retaining it.
Discovery requests time out and old-account responses cannot publish into a new
account. The service projection excludes physical paths and routing credentials.
