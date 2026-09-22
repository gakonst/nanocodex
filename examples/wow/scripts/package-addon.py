#!/usr/bin/env python3
"""Build a self-contained WoW addon ZIP, validating every manifest first."""
import argparse
from pathlib import Path, PurePosixPath
import zipfile

ROOT = Path(__file__).resolve().parent.parent


def addon_files(root=ROOT):
    addon = root / 'addon' / 'Nanocodex'
    # Explicit shipping surface: no caches, tests, credentials or source wrappers.
    files = {p.name: p for p in addon.iterdir()
             if p.is_file() and p.suffix in ('.lua', '.toc', '.xml')}
    files['Transport.lua'] = root / 'addon' / 'Transport.lua'
    if 'Nanocodex.toc' not in files:
        raise ValueError('Missing Nanocodex.toc')
    for name, path in files.items():
        if path.is_symlink() or not path.is_file():
            raise ValueError(f'Missing or symlinked addon file: {name}')
    for name, path in files.items():
        if path.suffix != '.toc':
            continue
        for line in path.read_text(encoding='utf-8-sig').splitlines():
            entry = line.strip()
            if not entry or entry.startswith('#'):
                continue
            relative = PurePosixPath(entry.replace('\\', '/'))
            if relative.is_absolute() or '..' in relative.parts or str(relative) not in files:
                raise ValueError(f'{name}: unavailable load entry {entry}')
    return files


def package(destination, root=ROOT):
    files = addon_files(root)  # Validate before opening or replacing an artifact.
    with zipfile.ZipFile(destination, 'w', compression=zipfile.ZIP_DEFLATED) as archive:
        for name, path in sorted(files.items()):
            info = zipfile.ZipInfo('Nanocodex/' + name, date_time=(1980, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o100644 << 16
            archive.writestr(info, path.read_bytes())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', nargs='?', type=Path, help='Destination ZIP')
    parser.add_argument('--check', action='store_true', help='Validate package without writing')
    args = parser.parse_args()
    try:
        if args.check:
            addon_files()
        elif args.output:
            package(args.output)
        else:
            parser.error('provide an output ZIP or --check')
    except (OSError, ValueError) as exc:
        parser.exit(1, f'Addon package failed: {exc}\n')


if __name__ == '__main__':
    main()
