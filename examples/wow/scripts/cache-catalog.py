#!/usr/bin/env python3
"""Seed a validated local addon catalog for the next UI reload. No credentials."""
import argparse
import hashlib
import os
from pathlib import Path


def write_catalog(client, snapshot):
    payload = snapshot.encode('utf-8')
    if not snapshot.startswith('ncw1\n') or len(payload) > 1024 * 1024:
        raise ValueError('bounded ncw1 catalog required')
    destination = Path(client) / 'Interface/AddOns/Nanocodex/Catalog.lua'
    if not destination.parent.is_dir():
        raise ValueError('install the addon first')
    # Decimal byte escapes prevent quotes, newlines and Lua delimiters in account
    # data from becoming source code. Projects.lua validates the parsed snapshot.
    literal = '"' + ''.join('\\%03d' % b for b in payload) + '"'
    revision = hashlib.sha256(payload).hexdigest()
    source = 'local _, NS = ...\nNS.CatalogCache = {revision="' + revision + '",snapshot=' + literal + '}\n'
    temporary = destination.with_suffix('.tmp')
    fd = os.open(temporary, os.O_CREAT | os.O_TRUNC | os.O_WRONLY | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'w') as stream:
        stream.write(source)
    os.replace(temporary, destination)
    return revision


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('client', type=Path)
    parser.add_argument('snapshot', type=Path)
    args = parser.parse_args()
    print(write_catalog(args.client, args.snapshot.read_text()))
