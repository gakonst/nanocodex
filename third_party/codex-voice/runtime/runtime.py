"""Prepare receipt-verified native libraries without changing their input trees."""

from collections.abc import Callable
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import re
import shutil

PLUGINS = (
    "app",
    "audioconvert",
    "audioresample",
    "coreelements",
    "opus",
    "rtp",
    "rtpmanager",
)


def required_library_paths(target: str) -> tuple[str, ...]:
    """Libraries needed by the bindings even when no selected plugin imports them."""
    if target.endswith("-apple-darwin"):
        return ("lib/libgio-2.0.0.dylib",)
    if target.endswith("-unknown-linux-gnu"):
        return ("lib/libgio-2.0.so.0",)
    if target.endswith("-pc-windows-msvc"):
        return ("bin/gio-2.0-0.dll",)
    raise ValueError("unsupported native runtime target")


@dataclass(frozen=True)
class Binary:
    identity: str
    imports: tuple[str, ...]
    rpaths: tuple[str, ...]


@dataclass(frozen=True)
class RuntimeFormat:
    """Platform loader policy; inspect reads metadata, finalize_copy transforms or checks a verified copy."""

    plugins: tuple[Path, ...]
    system_imports: frozenset[str]
    inspect: Callable
    finalize_copy: Callable
    library_dir: str = "lib"
    plugin_dir: str = "plugins"
    required_libraries: tuple[str, ...] = ()


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def prepare(prefix, receipts, target, output, format):
    prefix, receipts = prefix.resolve(strict=True), receipts.resolve(strict=True)
    output = output.absolute()
    if (
        output.exists()
        or output.is_symlink()
        or any(
            parent.exists() and parent.samefile(source)
            for parent in output.resolve().parents
            for source in (prefix, receipts)
        )
    ):
        raise ValueError("output must be fresh and outside the inputs")
    ci = json.loads((receipts / "ci.json").read_text())
    source_hash = digest(Path(__file__).with_name("sources.json"))
    if (
        ci.get("target") != target
        or ci.get("build_complete") is not True
        or ci.get("inspection_complete") is not True
        or ci.get("manifest_sha256") != source_hash
        or not re.fullmatch(r"[0-9a-f]{40}", ci.get("commit", ""))
    ):
        raise ValueError(
            "native build receipt does not match the pinned source inputs and target"
        )
    inventory_path = receipts / "inspection/binaries.json"
    inventory = json.loads(inventory_path.read_text())
    if not 1 <= len(inventory) <= 128:
        raise ValueError("unexpected native inventory size")
    binaries, identities, destinations = {}, {}, {}
    for record in inventory:
        spelling = (
            record["path"].replace("\\", "/") if os.name == "nt" else record["path"]
        )
        relative = Path(spelling)
        if relative.anchor or ".." in relative.parts or relative.as_posix() != spelling:
            raise ValueError("native inventory path must be canonical and relative")
        path = prefix / relative
        if (
            path.is_symlink()
            or not path.is_file()
            or not path.resolve().is_relative_to(prefix)
        ):
            raise ValueError(
                "native inventory entries must be regular files inside the prefix"
            )
        if record["target"] != target or digest(path) != record["sha256"]:
            raise ValueError(f"native input target/digest mismatch: {relative}")
        metadata = format.inspect(path, target)
        name = Path(metadata.identity).name
        if not re.fullmatch(r"[A-Za-z0-9_+.-]+", name):
            raise ValueError("invalid native library identity")
        if relative in format.plugins and name != relative.name:
            raise ValueError("explicit plugin identity must preserve its filename")
        if (
            metadata.identity in identities
            or metadata.identity in format.system_imports
            or relative in binaries
        ):
            raise ValueError("duplicate or system native library identity")
        destination = (
            Path(
                format.plugin_dir if relative in format.plugins else format.library_dir
            )
            / name
        )
        if any(
            destination.as_posix().casefold() == p.as_posix().casefold()
            for p in destinations.values()
        ):
            raise ValueError("colliding runtime library filenames")
        binaries[relative] = (record, metadata)
        identities[metadata.identity] = relative
        destinations[relative] = destination
    libraries = {path.name: relative for relative, path in destinations.items()}
    if any(name not in libraries for name in format.required_libraries):
        raise ValueError("missing required native library")
    pending = [
        *format.plugins,
        *(libraries[name] for name in format.required_libraries),
    ]
    selected = set()
    while pending:
        relative = pending.pop()
        if relative in selected:
            continue
        if relative not in binaries:
            raise ValueError(f"missing explicit plugin: {relative}")
        selected.add(relative)
        for dependency in binaries[relative][1].imports:
            if dependency in format.system_imports:
                continue
            if dependency not in identities:
                raise ValueError(f"undeclared native dependency: {dependency}")
            pending.append(identities[dependency])
    output.mkdir()  # Only this exclusively created output may be removed on failure.
    try:
        records = []
        dependency_paths = {
            identity: output / destinations[path]
            for identity, path in identities.items()
        }
        for relative in sorted(selected):
            record, metadata = binaries[relative]
            destination = output / destinations[relative]
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(prefix / relative, destination)
            if (
                digest(destination) != record["sha256"]
                or format.inspect(destination, target) != metadata
            ):
                raise ValueError("copied native input changed after verification")
            expected = format.finalize_copy(destination, metadata, dependency_paths)
            if format.inspect(destination, target) != expected:
                raise ValueError(
                    "finalized loader commands did not match the private runtime layout"
                )
            records.append(
                {
                    "path": destinations[relative].as_posix(),
                    "sourcePath": relative.as_posix(),
                    "sourceSha256": record["sha256"],
                    "sha256": digest(destination),
                    "imports": list(expected.imports),
                }
            )
        manifest = {
            "schemaVersion": 1,
            "developmentOnly": True,
            "target": target,
            "sourceCommit": ci["commit"],
            "sourceManifestSha256": source_hash,
            "inventorySha256": digest(inventory_path),
            "libraries": records,
            "plugins": [destinations[path].as_posix() for path in format.plugins],
        }
        (output / "runtime.json").write_text(
            json.dumps(manifest, indent=2) + "\n", encoding="utf-8"
        )
    except BaseException:
        shutil.rmtree(output)
        raise
