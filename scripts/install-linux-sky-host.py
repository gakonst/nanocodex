#!/usr/bin/env python3
"""Create a Linux launcher for an already installed, unmodified OpenAI CUA runtime.

Run the launcher as the desktop user with its DISPLAY and session bus available.
This installs Nanocodex transport files separately; it never downloads or patches
OpenAI binaries, supplies approvals, or disables the Codex model sandbox.
"""
import argparse
import json
import os
from pathlib import Path
import shlex
import shutil

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--runtime', type=Path, required=True, help='Installed cua_node directory')
parser.add_argument('--codex-cli', type=Path, required=True, help='Matching upstream Codex executable')
parser.add_argument('--destination', type=Path, required=True, help='New, private host installation directory')
parser.add_argument('--register-managed', action='store_true', help='Select this launcher for automatic Linux guest discovery')
parser.add_argument('--surfaces', choices=['computer', 'browser,computer'], default='browser,computer')
args = parser.parse_args()
runtime, codex, destination = args.runtime.resolve(), args.codex_cli.resolve(), args.destination.resolve()
modules = runtime / 'lib/node_modules'
provider = modules / '@oai/cua-repl/bin/cua-repl.mjs'
for executable in (runtime / 'bin/node', runtime / 'bin/node_repl', codex):
    if not executable.is_file() or not os.access(executable, os.X_OK):
        parser.error(f'Required upstream executable unavailable: {executable}')
for source in (provider, modules / '@oai/sky/dist/project/cua/sky_js/src/service.js'):
    if not source.is_file():
        parser.error(f'Required upstream module unavailable: {source}')
if destination == runtime or runtime in destination.parents:
    parser.error('Host destination must be outside the upstream runtime')
if destination.exists():
    parser.error('Use a new destination; running host installations are immutable')
source_dir = Path(__file__).resolve().parents[1] / 'crates/experimental/nanocodex-computer/src'
destination.mkdir(mode=0o700, parents=True)
for name in ('linux_sky_host.mjs', 'linux_sky_proxy.mjs', 'linux_sky_worker.mjs'):
    target = destination / name
    shutil.copyfile(source_dir / name, target)
    target.chmod(0o600)
variables = {
    'CODEX_CLI_PATH': str(codex),
    'CUA_REPL_NODE_REPL_PATH': str(runtime / 'bin/node_repl'),
    'CUA_REPL_ENABLED_SURFACES': args.surfaces,
    'NODE_REPL_NODE_PATH': str(runtime / 'bin/node'),
    'NODE_REPL_NODE_MODULE_DIRS': str(modules),
    'NODE_REPL_TRUSTED_CODE_PATHS': str(modules),
}
command = [str(runtime / 'bin/node'), str(destination / 'linux_sky_host.mjs'), str(provider)]
launcher = destination / 'cua-provider'
launcher.write_text('#!/bin/sh\nset -eu\n' + ''.join(f'export {key}={shlex.quote(value)}\n' for key,value in variables.items()) + 'exec ' + ' '.join(map(shlex.quote, command)) + ' "$@"\n')
launcher.chmod(0o700)
if args.register_managed:
    base = Path(os.environ.get('NANOCODEX_DIR') or Path.home() / '.nanocodex').resolve()
    managed = base / 'runtimes/openai-cua'
    managed.mkdir(mode=0o700, parents=True, exist_ok=True)
    receipt = {'status': 'installed', 'transport': 'mcp', 'executable': str(launcher),
               'args': [], 'environment': {}}
    # Readers see either the previous complete selection or this complete receipt.
    import tempfile
    with tempfile.NamedTemporaryFile(mode='w', dir=managed, prefix='.provider-', delete=False) as stage:
        json.dump(receipt, stage)
        stage.write('\n')
    try:
        os.replace(stage.name, managed / 'provider.json')
    finally:
        Path(stage.name).unlink(missing_ok=True)
print(launcher)
