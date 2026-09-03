# Nanocodex2 VM hand

`nanocodex2 hand` registers one retained libkrun VM as an account-scoped
execution hand. Any hosted agent in the account can use the VM through the
standard `exec_command`, `write_stdin`, `apply_patch`, and `view_image`
contracts over the existing outbound Hosted Tools WebSocket.

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

Set `NANOCODEX_MANAGED_URL` to connect the same binary to another Nanocodex
cluster. The API key determines account attachment authority and is not passed
into the guest. The hand initiates the only network connection, so it works
behind NAT without an inbound listener.

The raw ext4 root is modified in place and exclusively locked while attached.
It survives turns and reconnects, but its files are independent from the
brain's Cloudflare Computer workspace and its lazy Cloudflare Sandbox. Use
`--vm-no-network` for an offline guest. A directory root is supported as a
development escape hatch and must already contain
`/usr/local/bin/nanocodex-vm-guest`.

The immutable attachment snapshot publishes the guest workspace plus `vm`,
`linux`, shell/filesystem/process/PTY, network state, CPU count, and memory to
`accountInfo().machines`. Reconnecting the hand replaces its current account
attachment generation under the existing lease/fencing rules. Ctrl-C drains
admitted calls, syncs the guest filesystem, and stops the VM.
