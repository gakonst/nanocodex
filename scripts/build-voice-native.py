#!/usr/bin/env python3
"""Build and stage the isolated native voice helper next to a Nanocodex executable.

macOS uses the Rust libWebRTC host with its statically linked audio engine.
Other targets consume a verified upstream prepared runtime via --runtime.
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


def run(*args):
    result = subprocess.run([str(arg) for arg in args], text=True, capture_output=True)
    if result.returncode:
        sys.stderr.write(result.stdout + result.stderr)
        result.check_returncode()
    return result.stdout.strip()


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def stage_libwebrtc(helper, output, target):
    """The Rust host owns a static WebRTC engine; only OS libraries may be imported."""
    for dependency in dependencies(helper):
        if not dependency.startswith(("/usr/lib/", "/System/Library/")):
            raise ValueError(f"non-system libWebRTC dependency: {dependency}")
    (output / "bin").mkdir()
    packaged = output / "bin/nanocodex-voice-host"
    shutil.copy2(helper, packaged)
    run("codesign", "--force", "--sign", "-", packaged)
    (output / "licenses").mkdir()
    (output / "licenses/libwebrtc.md").write_text(run(packaged, "--licenses") + "\n")
    shutil.copy2(VENDOR / "LICENSE", output / "licenses/Apache-2.0.txt")
    manifest = {
        "engine": "libwebrtc", "linkage": "static", "target": target,
        "libwebrtc": "0.3.48", "webrtc-sys": "0.3.45",
        "nativeTag": "webrtc-89d790b",
        "helperBuildCommit": run(packaged, "--build-commit"),
    }
    for name in ["libwebrtc.json", "runtime.json", "manifest.json"]:
        (output / name).write_text(json.dumps(manifest, indent=2) + "\n")
    (output / "sources.json").write_text(json.dumps({
        "rust": "https://github.com/livekit/rust-sdks",
        "nativeTag": manifest["nativeTag"],
        "cargoLockSha256": digest(VENDOR / "Cargo.lock"),
    }, indent=2) + "\n")
    (output / "NOTICE.md").write_text(
        "# Native voice\n\nRust libwebrtc bindings (Apache-2.0) from LiveKit. "
        "The statically linked WebRTC engine and its third-party notices are in "
        "licenses/libwebrtc.md. Native versions are pinned in Cargo.lock.\n")


def stage_recorder(output, target):
    """Compile the local recorder at package build time, never on the user's machine."""
    architecture = {"aarch64": "arm64", "x86_64": "x86_64"}[target.split("-", 1)[0]]
    recorder = output / "bin/nanocodex-voice-recorder"
    run("xcrun", "swiftc", "-O", "-target", architecture + "-apple-macosx14.0",
        ROOT / "scripts/voice-recorder.swift",
        "-Xlinker", "-sectcreate", "-Xlinker", "__TEXT", "-Xlinker", "__info_plist",
        "-Xlinker", ROOT / "scripts/voice-recorder-Info.plist", "-o", recorder)
    for dependency in dependencies(recorder):
        if not dependency.startswith(("/usr/lib/", "/System/Library/")):
            raise ValueError(f"non-system recorder dependency: {dependency}")
    run("codesign", "--force", "--sign", "-", recorder)


def dependencies(path):
    return [line.strip().split(" (compatibility")[0] for line in run("otool", "-L", path).splitlines()[1:]]


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
    libwebrtc = target.endswith("-apple-darwin") and args.runtime is None
    if not target.endswith(("-apple-darwin", "-unknown-linux-gnu", "-pc-windows-msvc")):
        parser.error("native voice supports macOS, GNU Linux, and MSVC Windows targets")
    if args.runtime is None and not (libwebrtc and platform.system() == "Darwin"):
        parser.error("a prepared --runtime is required for non-macOS builds")
    package = "nanocodex-webrtc-voice-host" if libwebrtc else "nanocodex-voice-host"
    command = ["cargo", "build", "--manifest-path", str(VENDOR / "Cargo.toml"), "-p", package, "--locked"]
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
        if libwebrtc:
            stage_libwebrtc(helper, staged, target)
        elif args.runtime:
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
        if target.endswith("-apple-darwin"):
            stage_recorder(staged, target)
        if not libwebrtc:
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
