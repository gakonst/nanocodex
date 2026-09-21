#!/usr/bin/env python3
"""Install one boot-started host Hand; run as root, execute the daemon as its owner."""
import argparse
import json
import os
from pathlib import Path
import plistlib
import pwd
import shutil
import subprocess
import sys

ROOT = Path('/opt/nanocodex/hand')
LABEL = 'com.nanocodex.hand'


def service(system, user, home, managed_url=None, account_file=None):
    command = [str(ROOT / 'nanocodex2'), 'hand']
    environment = {'HOME': home}
    if managed_url:
        environment['NANOCODEX_MANAGED_URL'] = managed_url
    if account_file:
        environment['NANOCODEX_ACCOUNT_FILE'] = account_file
    if system == 'darwin':
        return Path('/Library/LaunchDaemons') / f'{LABEL}.plist', plistlib.dumps({
            'Label': LABEL, 'ProgramArguments': command, 'UserName': user,
            'EnvironmentVariables': {**environment, 'PATH': '/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin'},
            'RunAtLoad': True, 'KeepAlive': {'SuccessfulExit': False}, 'ThrottleInterval': 10,
            'StandardOutPath': str(ROOT / 'daemon.log'), 'StandardErrorPath': str(ROOT / 'daemon.log'),
        })
    if system != 'linux':
        raise ValueError('Boot service installation supports macOS and systemd Linux')
    quote = lambda value: json.dumps(value.replace('%', '%%'))
    return Path('/etc/systemd/system/nanocodex-hand.service'), f'''[Unit]
Description=Nanocodex host Hand
Wants=network-online.target
After=network-online.target
StartLimitIntervalSec=0

[Service]
Type=simple
User={user}
Environment={' '.join(quote(key + '=' + value) for key, value in environment.items())}
ExecStart={' '.join(map(quote, command))}
Restart=on-failure
RestartSec=10
TimeoutStopSec=90
KillMode=mixed
UMask=0077

[Install]
WantedBy=multi-user.target
'''.encode()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--user', required=True, help='Account owner who has run nanocodex2 login')
    parser.add_argument('--managed-url', default=os.getenv('NANOCODEX_MANAGED_URL'))
    parser.add_argument('--account-file', default=os.getenv('NANOCODEX_ACCOUNT_FILE') or (
        str(Path(os.environ['CODEX_HOME']) / 'nanocodex-account.json') if os.getenv('CODEX_HOME') else None))
    args = parser.parse_args()
    if os.geteuid() != 0:
        parser.error('Run with sudo to install a machine boot service')
    user = pwd.getpwnam(args.user)
    if user.pw_uid == 0:
        parser.error('The Hand must run as a non-root account owner')
    target, content = service(sys.platform, user.pw_name, user.pw_dir, args.managed_url, args.account_file)
    config = {'user': user.pw_name, 'home': user.pw_dir}
    manifest = ROOT / 'owner.json'
    for directory in [ROOT, *ROOT.parents]:
        if directory.is_symlink() or (directory.exists() and (
                directory.stat().st_uid != 0 or directory.stat().st_mode & 0o022)):
            parser.error('The service installation directory must be root-owned and not writable by others')
    for path in [manifest, target, ROOT / 'daemon.log', ROOT / 'nanocodex2']:
        if path.is_symlink() or (path.exists() and not path.is_file()):
            parser.error('Refusing an unexpected service installation file')
    if manifest.exists():
        if json.loads(manifest.read_text()) != config:
            parser.error('This machine Hand belongs to another user; remove that service first')
    elif target.exists():
        parser.error('Remove the existing Hand service before installing this one')
    binary = args.binary.resolve(strict=True)
    if not binary.is_file() or not os.access(binary, os.X_OK):
        parser.error('--binary must be an executable nanocodex2 binary')
    ROOT.mkdir(parents=True, exist_ok=True)
    if sys.platform == 'darwin':
        (ROOT / 'daemon.log').touch(mode=0o600, exist_ok=True)
    temporary = ROOT / 'nanocodex2.new'
    with temporary.open('xb') as output, binary.open('rb') as source:
        shutil.copyfileobj(source, output)
    temporary.chmod(0o755)
    temporary.replace(ROOT / 'nanocodex2')
    manifest.write_text(json.dumps(config) + '\n')
    target.write_bytes(content)
    target.chmod(0o644)
    run = lambda *command: subprocess.run(command, check=True)
    if sys.platform == 'darwin':
        subprocess.run(['launchctl', 'bootout', f'system/{LABEL}'], check=False)
        run('launchctl', 'enable', f'system/{LABEL}')
        run('launchctl', 'bootstrap', 'system', str(target))
    else:
        run('systemctl', 'daemon-reload')
        run('systemctl', 'enable', 'nanocodex-hand.service')
        run('systemctl', 'restart', 'nanocodex-hand.service')
    print('Boot service installed. It uses the owner’s saved login and VM configuration.')


if __name__ == '__main__':
    main()
