# nanocodex-managed

Native account-managed lifecycle backend for Nanocodex. The crate owns the
authenticated managed HTTP service, resumable durable event stream, and
optional reverse attachment of a caller-owned `Tools` recipe. The cloud owns
model execution and retained history; this crate never reads provider
credentials or application environment variables.

`ManagedClient::create_with_settings` sets the initial model policy atomically.
The existing `set_model`, `set_thinking`, `set_reasoning_mode`, and
`set_fast_mode` methods patch individual fields for subsequent turns.

Durable schedules are exposed through `triggers`, `trigger`, `put_trigger`,
and `delete_trigger`. `CronTriggerConfig` contains the complete cron expression,
timezone, prompt, enabled state, and `CronSessionMode` (`New` or `Continue`).
The client validates identifiers and input bounds; the managed service validates
schedule syntax and timezone semantics. `put_trigger` replaces a named schedule
using PUT. Schedule receipts include delivery timestamps and the last agent/turn.
