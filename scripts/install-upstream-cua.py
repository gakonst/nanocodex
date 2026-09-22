#!/usr/bin/env python3
"""Copy an installed OpenAI CUA runtime unchanged and create an opt-in MCP launcher.

Does not change app defaults, credentials, permissions, or the production Code Mode
engine. Provider binaries remain local and are never added to this repository.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source-app', type=Path, required=True)
parser.add_argument('--destination', type=Path, required=True)
parser.add_argument('--surfaces', choices=['computer', 'browser', 'browser,computer'], default='computer')
args = parser.parse_args()
source = args.source_app.resolve() / 'Contents/Resources/cua_node'
if not (source / 'manifest.json').is_file():
    parser.error('source app does not contain a CUA runtime manifest')
if not (source / 'lib/node_modules/@oai/cua-repl/bin/cua-repl.mjs').is_file():
    parser.error('legacy Sky-only runtimes are unsupported; update to the current ChatGPT desktop app')
destination = args.destination.resolve()
if destination == source or source in destination.parents:
    parser.error('destination must be outside the installed source runtime')
destination.mkdir(parents=True, exist_ok=True)
runtime = destination / 'cua_node'
if not runtime.exists():
    if sys.platform == 'darwin':
        subprocess.run(['/usr/bin/ditto', str(source), str(runtime)], check=True)
    else:
        shutil.copytree(source, runtime, symlinks=True)

def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()

files = []
for path in sorted(source.rglob('*')):
    relative = path.relative_to(source)
    copied = runtime / relative
    if path.is_symlink():
        if not copied.is_symlink() or path.readlink() != copied.readlink():
            raise RuntimeError(f'Copy differs at symlink {relative}')
        files.append({'path': str(relative), 'symlink': str(path.readlink())})
    elif path.is_file():
        expected = digest(path)
        if not copied.is_file() or digest(copied) != expected:
            raise RuntimeError(f'Copy differs at {relative}; existing copies are never overwritten')
        files.append({'path': str(relative), 'sha256': expected, 'bytes': path.stat().st_size})
extra = {str(p.relative_to(runtime)) for p in runtime.rglob('*') if p.is_file() or p.is_symlink()} - {v['path'] for v in files}
if extra:
    raise RuntimeError(f'Copy contains unexpected files: {sorted(extra)[:10]}')

modules = runtime / 'lib/node_modules'
provider = modules / '@oai/cua-repl/bin/cua-repl.mjs'
node = runtime / 'bin/node'
node_repl = runtime / 'bin/node_repl'
for executable in [node, node_repl]:
    if not os.access(executable, os.X_OK):
        raise RuntimeError(f'Required executable unavailable: {executable}')
variables = {
    'NODE_REPL_NODE_PATH': str(node),
    'NODE_REPL_NODE_MODULE_DIRS': str(modules),
    'NODE_REPL_TRUSTED_CODE_PATHS': str(modules),
}
# This selects the copied signed helper without changing its code or any OS
# permission. The matching signed service enforces its own host requirements.
sky_apps = list((modules / '@oai/sky').glob('**/Codex Computer Use.app'))
if len(sky_apps) == 1:
    variables['SKY_CUA_SERVICE_PATH'] = str(sky_apps[0])
    variables['NODE_REPL_UNTRUSTED_ENV_ALLOWLIST'] = 'SKY_CUA_SERVICE_PATH'
    if sys.platform == 'darwin':
        subprocess.run(['/usr/bin/codesign', '--verify', '--deep', '--strict', str(sky_apps[0])], check=True)
# Match the official host's Tab.ax capability used by browser tab lookup/creation.
variables.update(CUA_REPL_NODE_REPL_PATH=str(node_repl), CUA_REPL_ENABLED_SURFACES=args.surfaces,
                 BROWSER_USE_TINYSKY_ENABLED='1')
command = [str(node), str(provider)]
kind = 'cua-repl'
launcher = destination / 'cua-provider'
launcher.write_text('#!/bin/sh\nset -eu\n' +
                    'export PATH=' + shlex.quote(str(runtime / 'bin')) + ':"$PATH"\n' +
                    ''.join(f'export {key}={shlex.quote(value)}\n' for key, value in variables.items()) +
                    'exec ' + ' '.join(shlex.quote(value) for value in command) + '\n')
launcher.chmod(0o755)
receipt = {
    'source': str(source), 'copy': str(runtime), 'exact_copy': True,
    'provider_kind': kind, 'launcher': str(launcher), 'files': files,
    'runtime_manifest': json.loads((runtime / 'manifest.json').read_text()),
    'package_versions': {name: json.loads((modules / '@oai' / name / 'package.json').read_text())['version'] for name in ['cua', 'cua-repl', 'sky']},
    'host_services': 'The provider can still require its installed app-server, OS grants, and native service. Copying does not replace or bypass these.',
}
(destination / 'copy-manifest.json').write_text(json.dumps(receipt, indent=2) + '\n')
print(json.dumps({'verified_entries': len(files), 'provider_kind': kind, 'launcher': str(launcher), 'transport': 'mcp'}))
