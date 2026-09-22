#!/usr/bin/env python3
"""Build the pinned native libraries and publish a self-contained voice archive."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import shlex
import subprocess
import sys
import tarfile
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
UPSTREAM_COMMIT = "1427825c4044d48b513c7d4ea32b84e58806a188"


def run(*args, **kwargs):
    subprocess.run([str(arg) for arg in args], check=True, **kwargs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", type=Path, help="pinned GStreamer build tools (non-macOS)")
    parser.add_argument("--target", required=True)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.target.endswith("-apple-darwin"):
        package = args.work.resolve() / "package"
        run(sys.executable, ROOT / "scripts/build-voice-native.py", "--target", args.target,
            "--release", "--output", package)
        archive_voice(package, args.output)
        return
    if args.upstream is None:
        parser.error("--upstream is required for the GStreamer runtime")
    upstream = args.upstream.resolve(strict=True)
    revision = subprocess.check_output(["git", "-C", str(upstream), "rev-parse", "HEAD"], text=True).strip()
    if revision != UPSTREAM_COMMIT:
        parser.error(f"native build tools must be checked out at {UPSTREAM_COMMIT}")
    tools = upstream / "third_party/voice"
    manifest = (ROOT / "third_party/codex-voice/runtime/sources.json").read_bytes()
    if (tools / "sources.json").read_bytes() != manifest:
        parser.error("upstream native sources differ from the vendored manifest")
    work = args.work.resolve()
    work.mkdir(parents=True, exist_ok=True)
    archives = work / "archives"
    archives.mkdir(exist_ok=True)
    for source in json.loads(manifest)["sources"]:
        path = archives / source["archive"]
        if not path.exists():
            print(f"Fetching pinned {source['name']} {source['version']}", flush=True)
            for attempt in range(4):
                try:
                    with urllib.request.urlopen(source["url"], timeout=60) as response:
                        data = response.read(64 * 1024 * 1024 + 1)
                    break
                except OSError:
                    if attempt == 3:
                        raise
                    time.sleep(2 ** attempt)
            if len(data) > 64 * 1024 * 1024 or hashlib.sha256(data).hexdigest() != source["sha256"]:
                raise ValueError(f"source checksum mismatch: {source['name']}")
            path.write_bytes(data)
    native = work / "native"
    command = [sys.executable, tools / "build_native.py", "--archives", archives,
               "--output", native, "--target", args.target]
    for name, executable in [("cc", "cc"), ("cxx", "c++"), ("cmake", "cmake"),
                             ("make", "make"), ("pkg-config", "pkg-config"), ("shell", "sh")]:
        path = shutil.which(executable)
        if path is None:
            raise ValueError(f"missing native build tool: {executable}")
        command += [f"--{name}", path]
    if args.target.endswith("-apple-darwin"):
        command += ["--deployment-target", "14.0"]
    run(*command)
    sys.path.insert(0, str(tools))
    from prepare_built_runtime import prepare_built
    from release_runtime import seal
    status = work / "status.txt"
    status.write_text(f"STABLE_GIT_COMMIT {UPSTREAM_COMMIT}\n")
    runtime = work / "runtime"
    prepare_built(native / "prefix", native / "built.json", status, args.target, runtime)
    seal(runtime, args.target)
    package = work / "package"
    env = os.environ.copy()
    env["PKG_CONFIG_PATH"] = str(native / "prefix/lib/pkgconfig")
    # Native ALSA metadata can add /usr/lib before the private GLib search path.
    # Put the prepared runtime first so GStreamer never links an older system GLib.
    rustflags = env.get("CARGO_ENCODED_RUSTFLAGS")
    if rustflags is None:
        rustflags = "\x1f".join(shlex.split(env.pop("RUSTFLAGS", "")))
    env["CARGO_ENCODED_RUSTFLAGS"] = "-L\x1fnative=" + str(native / "prefix/lib")
    if rustflags:
        env["CARGO_ENCODED_RUSTFLAGS"] += "\x1f" + rustflags
    run(sys.executable, ROOT / "scripts/build-voice-native.py", "--runtime", runtime,
        "--target", args.target, "--release", "--output", package, env=env)
    voice = package / "nanocodex-resources/voice"
    shutil.copy2(tools / "sources.json", voice / "sources.json")
    (voice / "manifest.json").write_text(json.dumps({
        "sourceCommit": UPSTREAM_COMMIT,
        "helperBuildCommit": env.get("STABLE_GIT_COMMIT", "dev"),
        "target": args.target,
    }, indent=2) + "\n")
    archive_voice(package, args.output)


def archive_voice(package, output):
    voice = package / "nanocodex-resources/voice"
    output.parent.mkdir(parents=True, exist_ok=True)
    with tarfile.open(output, "w:gz", format=tarfile.USTAR_FORMAT) as archive:
        for path in sorted(voice.rglob("*")):
            if path.is_symlink():
                raise ValueError(f"runtime package contains a symbolic link: {path}")
            if path.is_file():
                archive.add(path, arcname=path.relative_to(package).as_posix(), recursive=False)
    print(f"Voice release archive: {output}")


if __name__ == "__main__":
    main()
