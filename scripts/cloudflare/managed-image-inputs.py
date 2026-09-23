#!/usr/bin/env python3
"""Resolve committed local Cargo inputs without downloading crates or running Cargo.

Optional and platform dependencies are intentionally retained: this is a safe
superset of the Linux release closure. Workspace manifests all affect resolution.
"""
import json
import pathlib
import re
import subprocess
import sys
import tomllib

ROOTS = ['Cargo.toml', 'Cargo.lock', 'bin', 'crates', 'examples', 'js/nanocodex', 'py/bindings', 'third_party']

def inputs(package):
    paths = subprocess.check_output(['git', 'ls-tree', '-rz', '--name-only', 'HEAD', '--', *ROOTS]).decode().split('\0')
    paths = set(filter(None, paths))
    manifests = sorted(p for p in paths if p == 'Cargo.toml' or p.endswith('/Cargo.toml'))
    # One batch avoids a process per source while retaining HEAD-only semantics.
    wanted = sorted(p for p in paths if p.endswith('.rs') or p.endswith('Cargo.toml'))
    data = subprocess.check_output(['git', 'cat-file', '--batch'], input=''.join('HEAD:' + p + '\n' for p in wanted).encode())
    contents = {}
    offset = 0
    for path in wanted:
        end = data.index(b'\n', offset)
        size = int(data[offset:end].split()[-1])
        contents[path] = data[end + 1:end + 1 + size].decode()
        offset = end + 2 + size
    def read(path):
        return contents[path]
    docs = {p: tomllib.loads(read(p)) for p in manifests}
    if 'Cargo.toml' not in docs:
        return []  # Small publication fixtures need not contain Rust sources.
    workspace = docs['Cargo.toml'].get('workspace', {}).get('dependencies', {})
    selected = set(manifests) | {'Cargo.lock'}
    visited = set()
    def normalize(base, path):
        import posixpath
        return posixpath.normpath(posixpath.join(base, path))
    def visit(manifest):
        if manifest in visited:
            return
        visited.add(manifest)
        doc = docs[manifest]
        base = str(pathlib.PurePosixPath(manifest).parent)
        selected.add(base)
        # Cargo permits binary/library entry points outside the package root.
        for target in [doc.get('lib', {}), *doc.get('bin', [])]:
            if 'path' in target:
                path = normalize(base, target['path'])
                selected.add(path if pathlib.PurePosixPath(path).parent.as_posix() == 'examples' else str(pathlib.PurePosixPath(path).parent))
        for table in [doc, *doc.get('target', {}).values()]:
            for kind in ['dependencies', 'build-dependencies']:
                for name, dep in table.get(kind, {}).items():
                    depbase = base
                    if isinstance(dep, dict) and dep.get('workspace'):
                        dep = workspace[name]
                        depbase = '.'
                    if isinstance(dep, dict) and 'path' in dep:
                        visit(normalize(depbase, dep['path']) + '/Cargo.toml')
    matches = [p for p, d in docs.items() if d.get('package', {}).get('name') == package]
    if len(matches) != 1:
        raise ValueError('expected one Cargo package: ' + package)
    visit(matches[0])
    # Follow literal external module and embedded asset references as well. Full
    # package directories already cover ordinary modules, build scripts and data.
    scanned = set()
    while True:
        sources = {p for p in paths if p.endswith('.rs') and any(p == s or p.startswith(s + '/') for s in selected)} - scanned
        if not sources:
            break
        for path in sources:
            scanned.add(path)
            source = read(path)
            parent = str(pathlib.PurePosixPath(path).parent)
            stem = pathlib.PurePosixPath(path).stem
            for module in re.findall(r'\bmod\s+([A-Za-z_][A-Za-z_0-9]*)\s*;', source):
                for base in [parent, normalize(parent, stem)]:
                    for suffix in [module + '.rs', module + '/mod.rs']:
                        candidate = normalize(base, suffix)
                        if candidate in paths:
                            selected.add(candidate)
            for ref in re.findall(r'#\[path\s*=\s*"([^"]+)"\]|include(?:_str|_bytes)?!\s*\(\s*"([^"]+)"', source):
                candidate = normalize(str(pathlib.PurePosixPath(path).parent), ref[0] or ref[1])
                if candidate in paths:
                    selected.add(candidate)
    return sorted(selected)

if __name__ == '__main__':
    print(json.dumps(inputs(sys.argv[1])))
