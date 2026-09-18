# Offline Omarchy upgrade and rollback

`upgrade-omarchy.py` upgrades an **existing** fixed installation at
`/opt/nanocodex/background-cua`. It does not perform first installation, alter
sudoers/systemd configuration, restart any process, or unload a compositor
plugin. Run it with Python 3 on Linux. It requires normal interactive root
privilege for apply/rollback; the companion's narrow sudo grant cannot authorize
this operation. The old `install-omarchy.sh` refuses existing installations and
is not an upgrade entry point.

The operator must log out of **all** Hyprland sessions and keep graphical logins
quiescent for the entire command, using an SSH or text console connection that
survives logout. A new compositor must not start during the operation. The tool
checks `/proc` before preparation and immediately before exchange, but process
scanning is not a login inhibitor and cannot eliminate a simultaneous login
race. It deliberately does not stop a display manager or kill/restart sessions.
Active sessions are a refusal, including during dry-run.

## Reviewed bundle

The manifest has schema `1`, `files` mapping the seven fixed artifact names to
SHA-256, `hyprland_sha256` pinning `/usr/bin/Hyprland`, and `compositor` containing
the tested `commit`, `version`, `abiHash`, and `dirty: false`. The operator passes
the separately reviewed manifest hash; do not calculate that argument from an
untrusted replacement manifest at install time. The tool verifies every copied
artifact again, compares staged and installed ABI metadata, and refuses a
compositor executable change. The updated activator checks the fresh session's
exact runtime ABI identity **before** a plugin load. Runtime library changes can
therefore refuse activation even after an otherwise valid offline copy. The
plugin's own ABI/transport readiness checks still apply.

On the reviewed Omarchy host, the prepared bundle and code are at:

```
/srv/nanocodex/workspace/background-cua/safe-upgrade-final/
  code/upgrade-omarchy.py
  code/activate-plugin.py
  code/test_upgrade.py
  bundle/
  manifest.json
  evidence/tests.txt
  evidence/live-dry-run.txt
```

Reviewed manifest SHA-256:
`7c95c54c71dfd7e0f40db633d1abbe1e9a0e14fe3008fbdb454151ce54c64a06`.
Candidate companion SHA-256:
`66da0d5959f4d61b3893e5c92ec67de210e286edd79441626d0f697005a92421`.
Candidate plugin SHA-256:
`774e1212996021d553d07076a1b03c100f682cbc8e4e231ce38f2b73637ef194`.
The bundle contains the reviewed stricter activator, so it has a different
activator hash than the original staging folder.

After the operator has logged out and prevented competing graphical login:

```sh
cd /srv/nanocodex/workspace/background-cua/safe-upgrade-final
sudo python3 code/upgrade-omarchy.py --stage bundle --manifest manifest.json \
  --manifest-sha256 7c95c54c71dfd7e0f40db633d1abbe1e9a0e14fe3008fbdb454151ce54c64a06 --dry-run
sudo python3 code/upgrade-omarchy.py --stage bundle --manifest manifest.json \
  --manifest-sha256 7c95c54c71dfd7e0f40db633d1abbe1e9a0e14fe3008fbdb454151ce54c64a06
```

Inspect the reviewed script before running it through sudo. Existing paths may
be service-user owned and not accessible to the desktop account; use a trusted
administrator console to access the staged bundle, or have the administrator
copy the reviewed files to a root-owned staging directory first. Do not broaden
the companion sudo grant or change workspace permissions to work around this.
A full dry-run needs root read access to the existing root-owned sudoers file;
an unprivileged dry-run can still demonstrate active-session refusal.

The command prints a unique `/opt/nanocodex/.background-cua-upgrade-*` directory.
Retain it. A complete `cp -a` copy preserves ownership, modes, ACLs, xattrs and
all unrelated installed files. Updated files retain their original ownership
and modes. A durable receipt records both complete file trees with hashes and
ownership/modes before the single Linux `RENAME_EXCHANGE`. The fixed installed
path never disappears. No fallback to a sequence of non-atomic renames exists.
The exchanged `previous/` directory is the original installation, including
`nanocodex2`, wrappers and sudoers. Existing staged `current/` is untouched.

Log in manually to a fresh desktop session after a successful upgrade. The next
companion launch performs the guarded plugin activation. Existing Hand and
companion processes are never restarted by this tool; existing executable
mappings continue until those processes exit. End existing CUA conversations
before the transition and create a fresh binding afterward. Verify advertised
lane capacity and background input/capture in that new session before claiming
the candidate is active. File installation alone is not end-to-end acceptance.

## Rollback and interrupted commands

Log out and keep graphical logins quiescent again. Replace `SLOT` with the exact
printed sibling directory:

```sh
sudo python3 code/upgrade-omarchy.py --rollback /opt/nanocodex/SLOT --dry-run
sudo python3 code/upgrade-omarchy.py --rollback /opt/nanocodex/SLOT
```

Rollback verifies the saved and installed trees against the durable receipt,
then atomically exchanges them. It retains the replaced candidate and is
idempotent: a second rollback recognizes the restored original and does not
switch forward again. A crash after exchange but before the success message
is recoverable from the same receipt. A failure before exchange leaves the
installation intact; if the receipt exists, rollback recognizes that state.
An incomplete preparation without a receipt has not reached exchange: retain
it for inspection and do not use it as a rollback source. If a tree differs
from the receipt or Hyprland changed, automatic recovery refuses and requires
manual review. Never manually delete or overwrite a backup to silence refusal.

## Tests

On Linux, run:

```sh
python3 -m unittest discover -s scripts/linux-background-cua/deployment -p 'test_*.py' -v
```

Tests operate in temporary fixtures as an ordinary user. They cover artifact
and manifest tampering, exact ABI/executable mismatch, symlink refusal, active
or newly appearing compositors, injected copy and exchange failures, recovery
from interruption immediately after exchange, unchanged ancillary file metadata,
dry-run, idempotent rollback, modified backup refusal, and runtime ABI rejection
before any plugin command. They neither install into `/opt` nor load a plugin.
