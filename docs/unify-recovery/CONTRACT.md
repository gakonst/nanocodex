# Unified Nanocodex contract (integration branch: unify-nanocodex)

Integration worktree: /Users/georgios/github/gakonst/nanocodex/.claude/worktrees/unify-nanocodex
Owner decisions (final): one branch, land together. nc shadows netcat unconditionally. The unified TUI keeps the UNION of
legacy + nanocodex2 TUI features, except Jaeger and `/trace`, which are removed everywhere. curl nanocodex.paradigm.xyz keeps
its Cloudflare zone redirect to raw GitHub master/install (no Worker route).

## Binary + dispatch
- ONE binary: package nanocodex-bin, [[bin]] nanocodex. The nanocodex2-bin package goes away (its module tree merges into
  nanocodex-bin).
- Mode chosen before clap from the argv[0] basename (std::env::args_os, NOT current_exe, which canonicalizes symlinks):
  nanocodex | nc | nanocodex2 -> MANAGED (today's nanocodex2 command tree; bare = managed nanocodex2 TUI);
  ncl -> LOCAL (today's nanocodex command tree; bare = local non-durable agent TUI).
  A leading `--local` forces LOCAL and is stripped (`nanocodex --local ...` == `ncl ...`).
- Hidden/internal entrypoints dispatch the same under every argv0: __device-hand (incl --service-protocol, --daemon,
  --describe, --check-permissions, --request-permissions), __vm-run-config, vm-run-config, __install-hand, __update-hand,
  *-host helpers, and anything launchd/systemd/Windows tasks/old updaters call by path.
- Conflicting commands: managed meanings win at top level for run, login, status, logout, auth(=account).
  Local headless run => `ncl run` (keep exit code 75 RetryableProcessExit and the JSONL contract). Harness subscription
  login => `ncl auth`. Nanocodex Connect => `nanocodex connect login|status|logout` (`ncl login|status|logout` stay
  Connect for compatibility). `hand` is merged: no subcommand = serve (managed backend flags); the N1 management commands
  (install/connect/menu-bar/menu-status/status/keep-awake/start/stop/restart/recover/permissions) become subcommands, with
  clap args_conflicts_with_subcommands. Commands unique to either tree stay reachable at top level in BOTH modes when
  unambiguous (update, install, setup, computer, tui, cookies, eval, cron, attach, ...).
- --version prints exactly one "Commit SHA:" line (update/local.rs parses it).

## Install layout + aliases
- ~/.nanocodex/bin/{nanocodex,nanocodex2,nc,ncl} are symlinks to ../current/<exe>. Windows keeps nanocodex.exe and
  nanocodex2.exe copies, adds nc.cmd / ncl.cmd shims (nanocodex.exe [--local] %*), and updates installer.iss.
- macOS (see the bundle section): versions/<key>/Nanocodex.app/Contents/MacOS/nanocodex2 is the real file; versions/<key>/nanocodex
  and nanocodex2 are relative symlinks to it. Linux and older layouts: versions/<key>/{nanocodex,nanocodex2} are identical bytes.
- Compatibility: every validator that requires the file name nanocodex2 today (hand_service validate_plist, the
  legacy_publisher matchers, the Linux recovery record, Windows ensure/validate_hand_args, launcher installed_root_for)
  must ACCEPT both nanocodex and nanocodex2 and paths inside Nanocodex.app/Contents/MacOS/, while still WRITING nanocodex2 names.

## Releases (old updaters must keep working)
- Build once per target and sign once. Publish the SAME bytes as nanocodex-<triple>[.gz] AND nanocodex2-<triple>[.gz]
  (plus the .exe pair on Windows); SHA256SUMS keeps both.
- macOS also publishes nanocodex-app-aarch64-apple-darwin.tar.gz containing Nanocodex.app (listed in SHA256SUMS).

## macOS app bundle + signing
- Nanocodex.app: CFBundleIdentifier com.nanocodex.hand, CFBundleName "Nanocodex", CFBundleExecutable nanocodex2,
  CFBundlePackageType APPL, LSUIElement true, CFBundleShortVersionString and CFBundleVersion from the build.
- CI: Developer ID Application from the secrets MACOS_DEVELOPER_ID_P12_BASE64, MACOS_DEVELOPER_ID_P12_PASSWORD and
  MACOS_DEVELOPER_ID_TEAM_ID (team C3Q4NN5ZQ8), imported into a temporary keychain. codesign --force --timestamp
  --identifier com.nanocodex.hand --entitlements nanocodex-vm.entitlements (NO hardened runtime and NO notarization yet).
  Verify codesign --verify --strict and that the designated requirement contains identifier com.nanocodex.hand and
  leaf[subject.OU] = team. Without the secrets (forks/PRs), sign ad hoc and label the artifact unsigned. Release and
  nightly on gakonst/nanocodex fail closed when the secrets are missing.
- Proven on this Mac: TCC keys bundle clients by bundle id (client_type=0) plus csreq. A cert-signed bundle with a fixed
  identifier keeps Screen Recording across different builds at different paths. Ad hoc (cdhash) signing never persists.
- The Hand daemon runs from the bundle executable: launchd ProgramArguments [<versions/key>/Nanocodex.app/Contents/MacOS/nanocodex2, "hand"].
- FAST LOCAL LOOP (owner's top priority): `nanocodex update --path target/debug/nanocodex` (and
  `nanocodex hand restart --executable <bare binary>`) wraps a bare binary into Nanocodex.app, signs it with
  NANOCODEX_CODESIGN_IDENTITY or an auto-detected "Developer ID Application" identity from the login keychain
  (--timestamp=none locally), installs and activates it, and restarts the Hand, adding only seconds and no CI. Without an
  identity: ad hoc plus the existing re-request-permissions prompt. --hand-binary becomes optional (same binary).

## Validation (AGENTS.md): black-box E2E through the real CLI; evidence under the ignored output/.


## AMENDMENT 1 (owner decision, supersedes the single-binary parts above)
Two binaries split by ROLE, built from the one merged crate (keep Stage A's tree merge):
1. `nanocodex` [[bin]]: the only user-facing CLI. It contains BOTH command trees and the ONE unified TUI (managed by
   default; `--local`/ncl = local agent), plus aliases nc/ncl and the nanocodex2 compatibility name (bin/nanocodex2 ->
   nanocodex, managed mode). It is never the Hand daemon and needs no signing or TCC grants. Rebuild-and-run with no prompts.
2. `nanocodex-hand` [[bin]]: the macOS/Linux/Windows Hand daemon only (today's `nanocodex2 hand` serve path plus
   the daemon-side internals: __device-hand incl --daemon/--describe/--check-permissions/--request-permissions/
   --service-protocol, __vm-run-config/vm-run-config VMM child, screen/input/CUA/audio/recording, *-host helpers,
   __install-hand/__update-hand as needed). It is small, stable and changes rarely.
- `nanocodex hand ...`: the management subcommands (install/connect/status/start/stop/restart/recover/permissions/
  menu-bar/keep-awake/list/forget/prune/stream/...) stay in the CLI. Serving or daemon work SHELLS OUT to the installed
  nanocodex-hand executable (exec, preserving argv and exit codes); the CLI never runs daemon code in-process.
- macOS: nanocodex-hand ships inside Nanocodex.app (CFBundleIdentifier com.nanocodex.hand, Developer ID, team C3Q4NN5ZQ8,
  no hardened runtime yet). For compatibility (launchd validate_plist, old updaters, rollback) the bundle executable keeps
  the FILE NAME nanocodex2 for now: Nanocodex.app/Contents/MacOS/nanocodex2 contains the nanocodex-hand program.
  launchd runs [<...>/Nanocodex.app/Contents/MacOS/nanocodex2, "hand"]. TCC grants attach only to this app.
- Hand updates are decoupled from CLI updates: a CLI update must NOT replace or restart the Hand unless the Hand
  bytes changed (compare the version/commit and sha). Store layout: versions/<key>/nanocodex (CLI) plus a separately keyed
  Hand install (e.g. hand-versions/<hand-key>/Nanocodex.app). An unchanged Hand keeps its current bundle path, so
  nothing churns.
- Releases: publish nanocodex-<t>[.gz] = CLI, nanocodex2-<t>[.gz] = the HAND binary (same name old updaters expect
  for the companion; it must answer `__device-hand --service-protocol` and `hand`), and on macOS
  nanocodex-app-aarch64-apple-darwin.tar.gz = signed Nanocodex.app. Windows: nanocodex.exe CLI + nanocodex2.exe Hand
  (name kept) + installer; Linux systemd keeps running <...>/nanocodex2 hand.
- Old installed updaters therefore keep working: they fetch both assets as today, and the companion is the Hand.
- Fast local loop: CLI = cargo build and run (nothing else). Hand = `nanocodex hand dev <path-to-built nanocodex-hand>`
  (or `hand restart --executable`) wraps, signs with NANOCODEX_CODESIGN_IDENTITY or the auto-detected Developer ID
  Application identity (--timestamp=none), installs into the hand store and restarts. About a second of overhead.
  Use the SAME identity as release (Developer ID) so the grant is shared; mixing identities re-prompts.

