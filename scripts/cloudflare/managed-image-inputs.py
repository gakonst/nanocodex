#!/usr/bin/env python3
"""Resolve committed local Cargo inputs without downloading crates or running Cargo.

Optional dependencies and unknown cfgs remain conservative. Both Docker Rust
builders run natively on Linux x86_64 GNU; only proven target mismatches are
excluded. Root workspace manifests remain inputs for Cargo resolution.
"""
import json
import pathlib
import re
import subprocess
import sys
import tomllib

ROOTS = ['Cargo.toml', 'Cargo.lock', 'bin', 'crates', 'examples', 'js/nanocodex', 'py/bindings', 'third_party']

def linux_cfg(platform):
    # Three-valued evaluation: unsupported predicates/syntax remain included.
    # Cargo target predicates do not depend on optional dependency activation.
    expression = re.fullmatch(r"cfg\s*\((.*)\)", platform.strip(), re.S)
    if expression is None:
        return None if re.match(r"cfg\b", platform.strip()) else platform == "x86_64-unknown-linux-gnu"
    text = expression[1].strip()
    tokens = []
    while text:
        match = re.match(r'\s*([A-Za-z_][A-Za-z_0-9]*|"(?:[^"\\]|\\.)*"|[(),=])', text)
        if not match:
            return None
        tokens.append(match[1])
        text = text[match.end():].strip()
    position = 0
    def take():
        nonlocal position
        token = tokens[position]
        position += 1
        return token
    def peek():
        return tokens[position] if position < len(tokens) else None
    def parse():
        name = take()
        if not re.fullmatch(r'[A-Za-z_][A-Za-z_0-9]*', name):
            raise ValueError("unknown cfg syntax")
        if peek() == "=":
            take()
            value = json.loads(take())
            known = {"target_arch": "x86_64", "target_os": "linux", "target_family": "unix",
                     "target_env": "gnu", "target_vendor": "unknown", "target_pointer_width": "64",
                     "target_endian": "little"}
            return known[name] == value if name in known else None
        if peek() == "(":
            take()
            values = []
            while peek() != ")":
                values.append(parse())
                if peek() != ",":
                    break
                take()
            if take() != ")":
                raise ValueError("unterminated cfg")
            if name == "all":
                return False if False in values else (None if None in values else True)
            if name == "any":
                return True if True in values else (None if None in values else False)
            if name == "not" and len(values) == 1:
                return None if values[0] is None else not values[0]
            return None
        return {"unix": True, "windows": False}.get(name)
    try:
        value = parse()
        return value if position == len(tokens) else None
    except (IndexError, ValueError, TypeError):
        return None

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
    docs = {}
    def document(path):
        if path not in docs:
            docs[path] = tomllib.loads(read(path))
        return docs[path]
    if 'Cargo.toml' not in manifests:
        return []  # Small publication fixtures need not contain Rust sources.
    root_workspace = document('Cargo.toml').get('workspace', {})
    # Literal Cargo workspace exclusions cover subtrees. Unknown glob syntax
    # stays conservative rather than accidentally hiding a root member.
    exclusions = [str(pathlib.PurePosixPath(path)) for path in root_workspace.get('exclude', [])
                  if not any(char in path for char in '*?[')]
    def excluded_workspace(path):
        return any(path == root or path.startswith(root + '/') for root in exclusions)
    selected = {p for p in manifests if not excluded_workspace(p)} | {'Cargo.lock'}
    visited = set()
    def normalize(base, path):
        import posixpath
        result = posixpath.normpath(posixpath.join(base, path))
        if result == '..' or result.startswith('../') or result.startswith('/'):
            raise ValueError('Rust image inputs must remain inside the repository')
        return result
    def select_path(path):
        if path in paths:
            selected.add(path)
        else:
            selected.update(p for p in paths if path == '.' or p.startswith(path + '/'))
    def workspace_for(base, doc):
        explicit = doc.get('package', {}).get('workspace')
        ancestors = [normalize(base, explicit)] if explicit is not None else [base, *map(str, pathlib.PurePosixPath(base).parents)]
        for ancestor in ancestors:
            manifest = normalize(ancestor, 'Cargo.toml')
            if manifest in manifests and 'workspace' in document(manifest):
                selected.add(manifest)
                return ancestor, document(manifest)['workspace'].get('dependencies', {})
        return '.', root_workspace.get('dependencies', {})
    def visit(manifest):
        if manifest in visited:
            return
        visited.add(manifest)
        doc = document(manifest)
        base = str(pathlib.PurePosixPath(manifest).parent)
        workspace_base, workspace = workspace_for(base, doc)
        build = doc.get('package', {}).get('build', False)
        has_build_script = normalize(base, 'build.rs') in paths or bool(build)
        prefix = '' if base == '.' else base + '/'
        for path in paths:
            if path.startswith(prefix):
                first = pathlib.PurePosixPath(path[len(prefix):]).parts[0]
                if has_build_script or first not in ('tests', 'benches'):
                    selected.add(path)
        if isinstance(build, str):
            select_path(normalize(base, build))
        # Production entry points may live in tests/benches or outside the crate.
        # Keep their parent subtree except for the repository's examples root.
        for target in [doc.get('lib', {}), *doc.get('bin', [])]:
            if 'path' in target:
                path = normalize(base, target['path'])
                select_path(path if pathlib.PurePosixPath(path).parent.as_posix() == 'examples' else str(pathlib.PurePosixPath(path).parent))
        # Docker's build host and output target are both x86_64 Linux GNU,
        # including build-dependency and proc-macro compilation.
        for platform, table in [(None, doc), *doc.get('target', {}).items()]:
            if platform is not None and linux_cfg(platform) is False:
                continue
            for kind in ['dependencies', 'build-dependencies']:
                for name, dep in table.get(kind, {}).items():
                    depbase = base
                    if isinstance(dep, dict) and dep.get('workspace'):
                        dep = workspace[name]
                        depbase = workspace_base
                    if isinstance(dep, dict) and 'path' in dep:
                        visit(normalize(depbase, dep['path']) + '/Cargo.toml')
    # Excluded workspaces are not selectable with the root cargo -p invocation;
    # they are parsed lazily if an actual local dependency reaches them.
    matches = [p for p in manifests if not excluded_workspace(p)
               and document(p).get('package', {}).get('name') == package]
    if len(matches) != 1:
        raise ValueError('expected one Cargo package: ' + package)
    visit(matches[0])
    # Follow ordinary modules and literal external source/asset references.
    # This restores production inputs even if they live in omitted test folders.
    scanned = set()
    while True:
        sources = {p for p in selected if p.endswith('.rs') and p in contents} - scanned
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
            for ref in re.finditer(r'(?:#\[path\s*=\s*|include(?:_str|_bytes)?!\s*\(\s*)(?:r(#{0,8}))?"([^"\n]+)"', source):
                candidate = normalize(parent, ref[2])
                if candidate in paths:
                    selected.add(candidate)
    return sorted(selected)

if __name__ == '__main__':
    print(json.dumps(inputs(sys.argv[1])))
