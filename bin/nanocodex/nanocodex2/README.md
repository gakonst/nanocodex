# Nanocodex2 hands

`nanocodex2 hand` registers browser and/or retained libkrun VM capabilities as
one account-scoped hand. Any hosted agent in the account can use them over the
existing outbound Hosted Tools WebSocket. The hand initiates the only control
connection, so it works behind NAT without an inbound listener.

## Residential browser egress

A browser-only hand is enough to run hosted browser work through the machine's
own internet connection:

```bash
NANOCODEX_API_KEY=ncx_live_... \
nanocodex2 hand \
  --browser \
  --machine-id home \
  --machine-name "Home browser"
```

The account catalog exposes this as `user_home_browser` and marks the machine
with `browser` and `browser-egress` capabilities. Chromium launches lazily in a
private temporary profile. Page, frame, worker, and subresource requests all
originate on the hand, so a hosted-agent proof can ask it to open an IP echo
page:

```text
Use the browser on Home browser to open
https://api.ipify.org?format=json and report the IP shown in the page.
```

That response is the hand's public egress IP rather than the managed brain's.
Use `--browser-executable PATH` when private browser discovery should be pinned
to an exact Chrome or Chromium binary.

## Retained VM compute

The existing VM form remains available. Hosted agents use it through the
standard `exec_command` and `write_stdin` process contracts. The logical cwd
selects the hand; inside the selected VM it is translated to that hand's native
workspace.

```bash
just build-vm-guest

NANOCODEX_API_KEY=ncx_live_... \
nanocodex2 hand \
  --vm /srv/nanocodex/build-root.ext4 \
  --vm-guest-runtime target/x86_64-unknown-linux-musl/debug/nanocodex-vm-guest \
  --vm-workspace /workspace \
  --vm-cpus 8 \
  --vm-memory-mib 16384 \
  --machine-id build-vm \
  --machine-name "Build VM"
```

Add `--browser` to the VM command to publish both compute and residential
browser egress from one hand. Set `NANOCODEX_MANAGED_URL` to connect the same
binary to another Nanocodex cluster. The API key determines account attachment
authority and is not passed into either execution environment.

The raw ext4 root is modified in place and exclusively locked while attached.
It survives turns and reconnects, but its files are independent from the
brain's Cloudflare Computer workspace and its lazy Cloudflare Sandbox. Use
`--vm-no-network` for an offline guest. A directory root is supported as a
development escape hatch and must already contain
`/usr/local/bin/nanocodex-vm-guest`.

The immutable attachment snapshot publishes the selected browser/VM
capabilities to `accountInfo().machines`. Reconnecting the hand replaces its
current account attachment generation under the existing lease/fencing rules.
Ctrl-C drains admitted calls, closes Chromium, syncs the guest filesystem when
present, and stops the VM.

The command emits structured lifecycle and call traces to stderr by default.
They include the machine ID, enabled capabilities, configured CPU/memory and
root-image size, connection and catalog state, and each call's ID, tool name,
outcome, and duration. Hand launch, hand shutdown, and each attachment call are
bounded spans, so long-running hands export them continuously. Command
arguments, output, credentials, remote failure reasons, machine names,
workspaces, and host paths are omitted. Use `--log-format json`, `--log-file PATH`, or
`--otel-endpoint URL` for standard JSON, retained-file, or OTLP output;
`--log-filter` and
`RUST_LOG` accept normal tracing filter directives.
