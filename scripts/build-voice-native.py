#!/usr/bin/env python3
"""Build and stage the isolated native voice helper next to a Nanocodex executable.

Release builds consume a verified upstream prepared runtime via --runtime.
On macOS, development builds can project the locally installed GStreamer SDK.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
VENDOR = ROOT / "third_party/codex-voice"
PLUGINS = "app audioconvert audioresample coreelements opus rtp rtpmanager".split()


def run(*args):
    result = subprocess.run([str(arg) for arg in args], text=True, capture_output=True)
    if result.returncode:
        sys.stderr.write(result.stdout + result.stderr)
        result.check_returncode()
    return result.stdout.strip()


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def dependencies(path):
    return [line.strip().split(" (compatibility")[0] for line in run("otool", "-L", path).splitlines()[1:]]


def stage_macos_sdk(helper, output):
    """Copy physical dylibs, rewrite only copies, and sign the relocated development closure."""
    prefix = Path(run("pkg-config", "--variable=prefix", "gstreamer-1.0"))
    plugins = Path(run("pkg-config", "--variable=pluginsdir", "gstreamer-1.0"))
    roots = [(helper, "bin/nanocodex-voice-host")]
    roots += [(plugins / f"libgst{name}.dylib", f"plugins/libgst{name}.dylib") for name in PLUGINS]
    roots += [(prefix / "lib/libgstreamer-1.0.0.dylib", "lib/libgstreamer-1.0.0.dylib")]
    pending = list(roots)
    copied, origins = {}, {}
    while pending:
        source, relative = pending.pop()
        source = source.resolve(strict=True)
        if source in copied:
            continue
        if relative in origins and origins[relative] != source:
            raise ValueError(f"colliding native library: {relative}")
        target = output / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)
        target.chmod(target.stat().st_mode | 0o200)
        origins[relative] = source
        copied[source] = target
        for dependency in dependencies(source):
            if dependency.startswith(("/usr/lib/", "/System/Library/")):
                continue
            if not dependency.startswith("/"):
                raise ValueError(f"SDK dependency must be absolute: {dependency}")
            path = Path(dependency)
            if path.resolve() != source:
                pending.append((path, f"lib/{path.name}"))
    for source, target in copied.items():
        if target.suffix == ".dylib":
            run("install_name_tool", "-id", f"@rpath/{target.name}", target)
        for dependency in dependencies(source):
            if dependency.startswith(("/usr/lib/", "/System/Library/")):
                continue
            resolved = Path(dependency).resolve()
            if resolved == source:
                continue
            replacement = "@loader_path/" + os.path.relpath(copied[resolved], target.parent)
            run("install_name_tool", "-change", dependency, replacement, target)
        # Strip SDK build-machine search paths; every private import is now loader-relative.
        lines = run("otool", "-l", target).splitlines()
        for index, line in enumerate(lines):
            if line.strip() == "cmd LC_RPATH":
                path = lines[index + 2].strip().split(" (offset")[0].removeprefix("path ")
                run("install_name_tool", "-delete_rpath", path, target)
        run("codesign", "--force", "--sign", "-", target)
    for target in copied.values():
        for dependency in dependencies(target):
            if not dependency.startswith(("@loader_path/", "@rpath/", "/usr/lib/", "/System/Library/")):
                raise ValueError(f"unrelocated dependency: {dependency}")
    (output / "development-runtime.json").write_text(json.dumps({
        "developmentOnly": True,
        "gstreamer": run("pkg-config", "--modversion", "gstreamer-1.0"),
        "files": {str(path.relative_to(output)): digest(path) for path in copied.values()},
    }, indent=2) + "\n")


def relocate_prepared_helper(helper, staged, target):
    """Bind the helper to the verified runtime, never its build-machine SDK."""
    if target.endswith("-apple-darwin"):
        for dependency in dependencies(helper):
            if dependency.startswith(("/usr/lib/", "/System/Library/")):
                continue
            candidates = list(staged.glob("lib/" + Path(dependency).name))
            if len(candidates) != 1:
                raise ValueError(f"helper import missing from prepared runtime: {dependency}")
            relative = os.path.relpath(candidates[0], helper.parent)
            run("install_name_tool", "-change", dependency, "@loader_path/" + relative, helper)
        lines = run("otool", "-l", helper).splitlines()
        for index, line in enumerate(lines):
            if line.strip() == "cmd LC_RPATH":
                path = lines[index + 2].strip().split(" (offset")[0].removeprefix("path ")
                run("install_name_tool", "-delete_rpath", path, helper)
        run("codesign", "--force", "--sign", "-", helper)
    elif target.endswith("-unknown-linux-gnu"):
        run("patchelf", "--set-rpath", "$ORIGIN/../lib", helper)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runtime", type=Path, help="verified prepared native runtime")
    parser.add_argument("--target", help="native helper target triple")
    parser.add_argument("--release", action="store_true")
    parser.add_argument("--output", type=Path, help="directory containing the Nanocodex executable")
    args = parser.parse_args()
    target = args.target or next(line.split(": ", 1)[1] for line in run("rustc", "-vV").splitlines() if line.startswith("host: "))
    profile = "release" if args.release else "debug"
    if not target.endswith(("-apple-darwin", "-unknown-linux-gnu", "-pc-windows-msvc")):
        parser.error("native voice supports macOS, GNU Linux, and MSVC Windows targets")
    if args.runtime is None and (platform.system() != "Darwin" or args.release):
        parser.error("a prepared --runtime is required for releases and non-macOS builds")
    command = ["cargo", "build", "--manifest-path", str(VENDOR / "Cargo.toml"), "-p", "nanocodex-voice-host", "--locked"]
    if args.release:
        command.append("--release")
    if args.target:
        command += ["--target", target]
    subprocess.run(command, cwd=ROOT, check=True)
    build = Path(os.environ.get("CARGO_TARGET_DIR", VENDOR / "target"))
    if not build.is_absolute():
        build = ROOT / build
    if args.target:
        build /= target
    helper = build / profile / ("nanocodex-voice-host.exe" if "windows" in target else "nanocodex-voice-host")
    output = (args.output or ROOT / "target" / profile).resolve()
    resources = output / "nanocodex-resources"
    resources.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="voice-stage-", dir=resources) as temporary:
        staged = Path(temporary) / "voice"
        staged.mkdir()
        if args.runtime:
            sys.path.insert(0, str(VENDOR / "runtime"))
            from package_runtime import runtime_files
            source = args.runtime.resolve(strict=True)
            files = runtime_files(source, target, public_release=args.release)
            for relative, expected in files.items():
                destination = staged / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(source / relative, destination)
                if digest(destination) != expected:
                    raise ValueError("runtime changed during staging")
            (staged / "bin").mkdir(exist_ok=True)
            packaged_helper = staged / "bin" / helper.name
            shutil.copy2(helper, packaged_helper)
            relocate_prepared_helper(packaged_helper, staged, target)
        else:
            stage_macos_sdk(helper, staged)
        shutil.copytree(VENDOR / "runtime/licenses", staged / "licenses")
        shutil.copy2(VENDOR / "runtime/NOTICE.md", staged / "NOTICE.md")
        destination = resources / "voice"
        # Replace only this script's dedicated generated runtime directory.
        if destination.exists():
            shutil.rmtree(destination)
        staged.rename(destination)
    print(f"Voice helper staged at {destination}")


if __name__ == "__main__":
    main()
