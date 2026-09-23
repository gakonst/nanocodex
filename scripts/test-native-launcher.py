#!/usr/bin/env python3
"""Exercise native launch setup without building either complete CLI or using auth."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

REPO = Path(__file__).resolve().parents[1]
MODULE = REPO / "bin/nanocodex/src/launcher.rs"


def run():
    with tempfile.TemporaryDirectory(prefix="native-launcher-test-") as scratch:
        temporary = Path(scratch).resolve()
        tests = temporary / "unit-tests"
        subprocess.run(["rustc", "--edition=2024", "--test", str(MODULE), "-o", str(tests)], check=True)
        subprocess.run([str(tests)], check=True)
        source = temporary / "probe.rs"
        # Same module and entrypoint order as the shipped binaries, before threads.
        source.write_text(
            '#[path = ' + json.dumps(str(MODULE)) + ']\nmod launcher;\n' + r'''
use std::{io::Write, os::unix::ffi::OsStrExt};
fn main() -> std::io::Result<()> {
    launcher::initialize_install_root();
    launcher::dispatch_update()?;
    let mut output = std::io::stdout().lock();
    let values = [std::env::var_os("NANOCODEX_DIR").unwrap_or_default(),
                  std::env::current_dir()?.into_os_string(),
                  std::env::current_exe()?.canonicalize()?.into_os_string()];
    for value in values.into_iter().chain(std::env::args_os()) {
        output.write_all(value.as_bytes())?;
        output.write_all(b"\0")?;
    }
    output.flush()?;
    std::process::exit(23);
}
''')
        probe = temporary / "probe"
        subprocess.run(["rustc", "--edition=2024", "-O", "-C", "strip=symbols", str(source), "-o", str(probe)], check=True)
        assert b"NANOCODEX_NATIVE_LAUNCHER_V1" in probe.read_bytes(), "optimized binary lost capability"
        original = temporary / "original install"
        (original / "versions/pinned").mkdir(parents=True)
        (original / "updater").mkdir()
        (original / "bin").mkdir()
        (original / "current").symlink_to("versions/pinned")
        for name in ("nanocodex", "nanocodex2"):
            shutil.copy2(probe, original / "versions/pinned" / name)
            (original / "bin" / name).symlink_to("../current/" + name)
        shutil.copy2(probe, original / "updater/nanocodex")
        root = temporary / "moved install"
        original.rename(root)
        (temporary / "linked bin").symlink_to(root / "bin")
        arguments = [b"a b", b"", b"*.txt", b"--flag", b"line\nbreak", b"non-utf8-\xff"]
        count = 0

        def launch(command, cwd, path, args=arguments, expected=None, root_env=b"must be replaced"):
            nonlocal count
            env = os.environ.copy()
            env.update(PATH=str(path), CDPATH=str(temporary), NANOCODEX_DIR=os.fsdecode(root_env))
            result = subprocess.run([os.fsencode(command), *args], cwd=cwd, env=env, capture_output=True, timeout=5)
            assert result.returncode == 23, (command, result.returncode, result.stderr)
            values = result.stdout.split(b"\0")
            assert values[-1] == b"", values
            actual_root, actual_cwd, executable, argv0, *actual_args = values[:-1]
            assert actual_root == os.fsencode(root), (actual_root, root)
            assert actual_cwd == os.fsencode(cwd), (actual_cwd, cwd)
            assert actual_args == args, actual_args
            assert argv0 == os.fsencode(command), (argv0, command)
            if expected is not None:
                assert executable == os.fsencode(expected), (executable, expected)
            count += 1

        for name in ("nanocodex", "nanocodex2"):
            for command, cwd, path in (
                (root / "bin" / name, temporary, ""),
                ("moved install/bin/../bin//" + name, temporary, ""),
                (temporary / "linked bin" / name, temporary, ""),
                (name, temporary, str(root / "bin") + "/"),
                (name, temporary, "moved install/bin/"),
                (name, root / "bin", ""),
            ):
                launch(command, cwd, path, expected=root / "versions/pinned" / name)

        legacy = root / "bin/nanocodex"
        launch(legacy, temporary, "", args=[b"update", *arguments], expected=root / "versions/pinned/nanocodex")
        marker = root / "updater/nanocodex.sha256"
        marker.write_text("present\n")
        launch(legacy, temporary, "", args=[b"update", *arguments], expected=root / "updater/nanocodex")
        launch(legacy, temporary, "", args=[b"--version"], expected=root / "versions/pinned/nanocodex")
        launch(legacy, temporary, "", args=[b"--", b"update"], expected=root / "versions/pinned/nanocodex")
        launch(root / "updater/nanocodex", temporary, "", args=[b"update"], expected=root / "updater/nanocodex")
        (root / "updater/nanocodex").unlink()
        os.link(root / "versions/pinned/nanocodex", root / "updater/nanocodex")
        launch(legacy, temporary, "", args=[b"update"], expected=root / "versions/pinned/nanocodex")
        (root / "updater/nanocodex").unlink()
        missing = subprocess.run([legacy, "update"], capture_output=True, timeout=5)
        assert missing.returncode != 0, "a present marker must not silently run an absent updater"
        # Executing a pinned version still discovers the root after current moves.
        (root / "versions/new").mkdir()
        (root / "current").unlink()
        (root / "current").symlink_to("versions/new")
        marker.unlink()
        launch(root / "versions/pinned/nanocodex", temporary, "", expected=root / "versions/pinned/nanocodex")
        standalone = subprocess.run([probe], env={**os.environ, "NANOCODEX_DIR": "explicit-dev-root"}, capture_output=True, timeout=5)
        assert standalone.returncode == 23
        assert standalone.stdout.split(b"\0")[0] == b"explicit-dev-root"
        print(f"native launcher: {count} installed process cases, missing-updater failure, standalone env, and optimized marker passed")


if __name__ == "__main__":
    if os.name != "posix":
        raise SystemExit("native launcher tests require Unix")
    run()
