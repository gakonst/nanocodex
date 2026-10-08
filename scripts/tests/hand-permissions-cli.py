#!/usr/bin/env python3
"""Read-only macOS Hand permission journey against the real installed daemon.

Run with --cli /absolute/path/to/nanocodex --evidence /ignored/output/directory.
Requires a running Hand built with the status-check protocol. Never requests or
changes OS permissions; the interactive grant/restart journey is separate.
"""
import argparse
import json
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--cli', type=Path, required=True)
parser.add_argument('--evidence', type=Path, required=True)
args = parser.parse_args()
args.evidence.mkdir(parents=True, exist_ok=True)
receipt = []


def run(arguments, success):
    command = [str(args.cli.resolve()), 'hand', *arguments]
    result = subprocess.run(command, capture_output=True, text=True, timeout=30)
    receipt.append({'command': command, 'expected_success': success,
                    'returncode': result.returncode, 'stdout': result.stdout,
                    'stderr': result.stderr})
    (args.evidence / 'cli-journey.json').write_text(json.dumps(receipt, indent=2) + '\n')
    assert (result.returncode == 0) == success, receipt[-1]
    return result.stdout


for flags in [['--json'], ['--guide', '--check'], ['--open-settings', '--check']]:
    run(['permissions', *flags], False)
service = json.loads(run(['status'], True))
assert service['loaded'] and service['pid'], service
for _ in range(2):
    status = json.loads(run(['permissions', '--check', '--json'], True))
    assert status['schema_version'] == 1, status
    assert status['daemon'] == {'pid': service['pid'], 'executable': service['executable']}, status
    for name, pane in [('input', 'Privacy_Accessibility'), ('screenCapture', 'Privacy_ScreenCapture')]:
        permission = status['permissions'][name]
        assert type(permission['granted']) is bool, status
        assert permission['pane'] == pane, status
run(['permissions', '--check'], True)
print('CLI permission journey passed; commands and observed results:', args.evidence / 'cli-journey.json')
