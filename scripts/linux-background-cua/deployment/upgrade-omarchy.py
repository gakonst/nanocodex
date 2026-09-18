#!/usr/bin/env python3
"""Offline-only upgrade/rollback. Never invokes sudo, systemctl or hyprctl."""
import argparse
import ctypes
import fcntl
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile

ROOT = Path('/opt/nanocodex/background-cua')
BINARY = Path('/usr/bin/Hyprland')
FILES = {'activate-plugin.py', 'compositor-version.json', 'cua-hyprland-plugin.so',
         'desktop-companion', 'hand-companion', 'nanocodex-computer', 'nanocodex-hyprland-capture'}
ABI_KEYS = ('commit', 'version', 'abiHash', 'dirty')


def require(ok, message):
    if not ok:
        raise RuntimeError(message)


def sha(path):
    h = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()


def regular(path):
    require(stat.S_ISREG(path.lstat().st_mode), f'Not a regular file: {path}')


def abi(value):
    require(all(k in value for k in ABI_KEYS) and value['dirty'] is False,
            'Missing exact ABI identity or dirty compositor build')
    require(all(isinstance(value[k], str) and value[k] for k in ABI_KEYS[:3]), 'Invalid ABI identity')
    return {k: value[k] for k in ABI_KEYS}


def no_compositor(proc=Path('/proc')):
    require(proc.is_dir(), 'Linux /proc is required')
    active = []
    for process in proc.iterdir():
        if not process.name.isdigit():
            continue
        try:
            name = (process / 'comm').read_text().strip().lower()
            if name.startswith('hyprland'):
                active.append(process.name)
        except FileNotFoundError:
            continue  # Exited during enumeration.
        except PermissionError as error:
            raise RuntimeError('Cannot inspect every process; refusing upgrade') from error
    require(not active, 'Active Hyprland compositor(s): ' + ','.join(active) +
            '. Log out first; keep graphical logins quiescent throughout the operation.')


def inventory(root):
    require(root.is_dir() and not root.is_symlink(), f'Not a real directory: {root}')
    result = {}
    for path in [root, *sorted(root.rglob('*'))]:
        info = path.lstat()
        require(stat.S_ISREG(info.st_mode) or stat.S_ISDIR(info.st_mode),
                f'Special file or symlink refused: {path}')
        result[str(path.relative_to(root))] = {
            'mode': stat.S_IMODE(info.st_mode), 'uid': info.st_uid, 'gid': info.st_gid,
            'sha256': sha(path) if path.is_file() else None}
    return result


def exchange(left, right):
    # Linux RENAME_EXCHANGE keeps the fixed installed path present at every instant.
    libc = ctypes.CDLL(None, use_errno=True)
    require(hasattr(libc, 'renameat2'), 'renameat2 is required; no unsafe fallback')
    result = libc.renameat2(-100, os.fsencode(left), -100, os.fsencode(right), 2)
    if result:
        raise OSError(ctypes.get_errno(), 'Atomic directory exchange failed')


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def durable_tree(root):
    for path in root.rglob('*'):
        if path.is_file():
            with path.open('rb') as stream:
                os.fsync(stream.fileno())
    for path in sorted((p for p in root.rglob('*') if p.is_dir()), reverse=True):
        sync_dir(path)
    sync_dir(root)


def validate(stage, manifest, digest, root=ROOT, binary=BINARY):
    regular(manifest)
    require(sha(manifest) == digest, 'Manifest SHA-256 differs from reviewed value')
    data = json.loads(manifest.read_text())
    require(data.get('schema') == 1 and set(data['files']) == FILES, 'Unexpected artifact set')
    require(sha(binary) == data['hyprland_sha256'], 'Installed Hyprland binary changed; rebuild/revalidate')
    expected = abi(data['compositor'])
    for name, digest in data['files'].items():
        regular(stage / name)
        require(sha(stage / name) == digest, f'Artifact hash mismatch: {name}')
    require(abi(json.loads((stage / 'compositor-version.json').read_text())) == expected,
            'Staged ABI differs from reviewed manifest')
    require(abi(json.loads((root / 'compositor-version.json').read_text())) == expected,
            'Installed ABI differs; this tool cannot migrate compositor versions')
    inventory(root)
    return data


def install(stage, manifest, digest, root=ROOT, binary=BINARY):
    data = validate(stage, manifest, digest, root, binary)
    no_compositor()
    before = inventory(root)
    slot = Path(tempfile.mkdtemp(prefix='.background-cua-upgrade-', dir=root.parent))
    previous = slot / 'previous'
    print(f'Recovery/rollback directory: {slot}', flush=True)
    # Preserve every installed file, ownership, mode, ACL and xattr, including
    # nanocodex2 and sudoers. No mutation of the live directory during preparation.
    subprocess.run(['/usr/bin/cp', '-a', '--', str(root), str(previous)], check=True)
    for name in sorted(FILES):
        target = previous / name
        regular(target)
        metadata = target.stat()
        shutil.copyfile(stage / name, target)
        os.chown(target, metadata.st_uid, metadata.st_gid)
        os.chmod(target, stat.S_IMODE(metadata.st_mode))
        require(sha(target) == data['files'][name], f'Copied artifact changed: {name}')
    after = inventory(previous)
    receipt = {'schema': 1, 'before': before, 'after': after,
               'hyprland_sha256': data['hyprland_sha256'], 'manifest_sha256': digest}
    with (slot / 'receipt.json').open('x') as stream:
        json.dump(receipt, stream, indent=2)
        stream.flush()
        os.fsync(stream.fileno())
    durable_tree(previous)
    sync_dir(slot)
    sync_dir(root.parent)
    # Recheck after slow copies, immediately before the only live mutation.
    require(inventory(root) == before, 'Installed files changed during preparation')
    require(sha(binary) == data['hyprland_sha256'], 'Hyprland changed during preparation')
    no_compositor()
    exchange(root, previous)
    # Even if interrupted here, the durable receipt identifies both directory states.
    sync_dir(root.parent)
    sync_dir(slot)
    print(f'Installed. Prior installation retained at {previous}. Start a fresh desktop session manually.')


def rollback(slot, root=ROOT, binary=BINARY, dry_run=False):
    require(slot.parent == root.parent and slot.name.startswith('.background-cua-upgrade-'),
            'Rollback directory must be a sibling created by this tool')
    require(not slot.is_symlink() and slot.is_dir(), 'Invalid rollback directory')
    require(slot.stat().st_uid == root.stat().st_uid and stat.S_IMODE(slot.stat().st_mode) == 0o700,
            'Rollback directory must be root-owned mode 0700')
    regular(slot / 'receipt.json')
    receipt = json.loads((slot / 'receipt.json').read_text())
    require(receipt.get('schema') == 1, 'Invalid receipt schema')
    require(sha(binary) == receipt['hyprland_sha256'], 'Hyprland changed; rollback ABI must be revalidated')
    no_compositor()
    current, saved = inventory(root), inventory(slot / 'previous')
    if current == receipt['before'] and saved == receipt['after']:
        print('Original installation already active (not exchanged, or already rolled back).')
        return
    require(current == receipt['after'] and saved == receipt['before'],
            'Installation/backup differs from receipt; refusing automatic recovery')
    if dry_run:
        print('Rollback validated; no files changed.')
        return
    no_compositor()
    exchange(root, slot / 'previous')
    sync_dir(root.parent)
    sync_dir(slot)
    print('Original installation restored. Replaced candidate retained in rollback directory.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument('--stage', type=Path)
    action.add_argument('--rollback', type=Path)
    parser.add_argument('--manifest', type=Path)
    parser.add_argument('--manifest-sha256')
    parser.add_argument('--dry-run', action='store_true')
    args = parser.parse_args()
    require(sys.platform == 'linux', 'Linux only')
    require(args.dry_run or os.geteuid() == 0, 'Root required; invoke through normal interactive sudo')
    # Fixed paths prevent broadening the privileged installation boundary via CLI.
    require(ROOT.parent.stat().st_uid == 0 and not (ROOT.parent.stat().st_mode & 0o022),
            'Install parent must be root-owned and not group/world writable')
    require(ROOT.stat().st_uid == 0 and not (ROOT.stat().st_mode & 0o022),
            'Install root must be root-owned and not group/world writable')
    if args.dry_run:
        if args.rollback:
            rollback(args.rollback.absolute(), dry_run=True)
        else:
            require(args.manifest and args.manifest_sha256, 'Manifest and reviewed hash required')
            no_compositor()
            validate(args.stage, args.manifest, args.manifest_sha256)
            print('Upgrade validated; no files changed. Apply requires root and quiescent graphical logins.')
        return
    lock_path = ROOT.parent / '.background-cua-upgrade.lock'
    fd = os.open(lock_path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'r+') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        if args.rollback:
            rollback(args.rollback.absolute())
        else:
            require(args.manifest and args.manifest_sha256, 'Manifest and reviewed hash required')
            install(args.stage, args.manifest, args.manifest_sha256)


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, KeyError, RuntimeError, subprocess.SubprocessError) as error:
        sys.exit(f'Refused/failed: {error}. No compositor was unloaded or restarted. '
                 'If a recovery directory was printed, retain it and inspect with --rollback --dry-run.')
