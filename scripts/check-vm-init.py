#!/usr/bin/env python3
"""Reject host releases whose embedded libkrun init needs a guest ELF loader."""

from pathlib import Path
import struct
import sys


def verify(path: Path) -> None:
    data = path.read_bytes()
    if data[:7] != b"\x7fELF\x02\x01\x01":
        raise ValueError(f"{path}: VM init must be a little-endian ELF64 executable")
    offset = struct.unpack_from("<Q", data, 32)[0]
    size, count = struct.unpack_from("<HH", data, 54)
    if size != 56 or not count or offset + size * count > len(data):
        raise ValueError(f"{path}: invalid ELF program headers")
    for index in range(count):
        if struct.unpack_from("<I", data, offset + index * size)[0] == 3:
            raise ValueError(f"{path}: dynamically linked VM init; install the musl target before building")
    print(f"Static VM init verified: {path}")


if __name__ == "__main__":
    paths = list(Path(sys.argv[1]).glob("krun-init-blob-*/out/init"))
    if not paths:
        sys.exit("No embedded VM init found in the host build")
    for path in paths:
        verify(path)
