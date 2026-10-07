#!/usr/bin/env python3
"""Check legacy TUI log destinations using the real CLI and isolated homes.

python3 scripts/tests/tui-log-location-journey.py --binary target/debug/nanocodex
Terminal transcripts and log files remain in ignored output/ for inspection.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import shlex
import shutil
import struct
import subprocess
import termios
import time
import threading
from uuid import uuid4


def launch(command, env, cwd, registration_dir, transcript_path, barrier=None):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 32, 140, 0, 0))
    child = subprocess.Popen(command, cwd=cwd, env=env, stdin=slave,
                             stdout=slave, stderr=slave, start_new_session=True)
    os.close(slave)
    transcript = bytearray()
    ready = False
    identity = None
    deadline = time.monotonic() + 20
    try:
        while time.monotonic() < deadline:
            if select.select([master], [], [], 0.05)[0]:
                try:
                    chunk = os.read(master, 65536)
                except OSError as error:
                    if error.errno == errno.EIO:
                        break
                    raise
                if not chunk:
                    break
                transcript.extend(chunk)
                if b"\x1b[6n" in chunk:
                    os.write(master, b"\x1b[1;1R")
            if not ready:
                for path in registration_dir.glob("*.json"):
                    try:
                        registration = json.loads(path.read_text())
                        ready = registration.get("pid") == child.pid and bool(registration.get("active_session_id"))
                    except (ValueError, FileNotFoundError):
                        continue
                    if ready:
                        identity = {"pid": child.pid, "session_id": registration["active_session_id"]}
                        if barrier is not None:
                            # Neither TUI exits until both are alive and initialized.
                            barrier.wait(timeout=10)
                        os.write(master, b"\x04")
                        break
            if child.poll() is not None:
                break
        child.wait(timeout=3)
        assert ready, "TUI never became ready; inspect terminal transcript"
        assert child.returncode == 0, f"TUI exit: {child.returncode}"
        return identity
    finally:
        if child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=3)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
        os.close(master)
        transcript_path.write_bytes(re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", bytes(transcript)))


def default_log(directory, identity):
    files = list(directory.glob(f"tui-{identity['pid']}-*.log"))
    assert len(files) == 1, f"expected one log for PID {identity['pid']}, found {files}"
    return files[0]


def assert_identity(path, identity, workspace):
    fields = [json.loads(line).get("fields", {}) for line in path.read_text().splitlines()]
    assert any(row.get("pid") == identity["pid"]
               and row.get("session.id") == identity["session_id"]
               and row.get("workspace") == str(workspace.resolve()) for row in fields), path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", type=Path, default=Path("output/tui-log-location") / uuid4().hex)
    args = parser.parse_args()
    binary, artifact = args.binary.resolve(), args.output.resolve()
    artifact.mkdir(parents=True)
    source_binary = binary
    binary = artifact / "nanocodex"
    shutil.copy2(source_binary, binary)
    checks = []
    outcome = {"success": False, "source_binary": str(source_binary), "checks": checks}
    try:
        for case in ("xdg", "home", "empty-xdg", "relative-xdg", "env-override", "flag-override", "concurrent"):
            root = artifact / case
            launch_dir, workspace, home = [root / name for name in ("launch", "workspace", "home")]
            for directory in (launch_dir, workspace, home):
                directory.mkdir(parents=True)
            (launch_dir / ".env").write_text("")
            env = {"HOME": str(home), "CODEX_HOME": str(home / ".codex"),
                   "NANOCODEX_DIR": str(home / ".nanocodex"), "NANOCODEX_COMPUTER": "off",
                   "TERM": "xterm-256color", "NANOCODEX_LOG_FORMAT": "json",
                   "PATH": os.environ.get("PATH", "/usr/bin:/bin")}
            command = [str(binary), "--cwd", str(workspace), "--api-key", "synthetic-test-key",
                       "--websocket-url", "ws://127.0.0.1:1", "--api-base-url", "http://127.0.0.1:1", "--browser=none", "--mcp-defaults",
                       "false", "--web-search", "false", "--image-generation", "false"]
            log_dir = home / ".local/state/nanocodex/logs"
            expected = None
            if case == "xdg":
                env["XDG_STATE_HOME"] = str(root / "user-state")
                log_dir = root / "user-state/nanocodex/logs"
            elif case == "empty-xdg":
                env["XDG_STATE_HOME"] = ""
            elif case == "relative-xdg":
                env["XDG_STATE_HOME"] = "relative-state"
            elif case.endswith("override"):
                # An unusable default must not prevent an explicit destination.
                blocked = root / "blocked-state"
                blocked.write_text("this is a file, not a directory\n")
                env["XDG_STATE_HOME"] = str(blocked)
                env["NANOCODEX_LOG_FILE"] = "custom/env.log"
                expected = launch_dir / "custom/env.log"
                if case == "flag-override":
                    command += ["--log-file", "custom/flag.log"]
                    expected = launch_dir / "custom/flag.log"
            registration_dir = home / ".codex/nanocodex/tui/instances"
            if case == "concurrent":
                second_workspace = root / "second-workspace"
                second_workspace.mkdir()
                second_command = list(command)
                second_command[second_command.index("--cwd") + 1] = str(second_workspace)
                barrier = threading.Barrier(2)
                with ThreadPoolExecutor(max_workers=2) as pool:
                    first = pool.submit(launch, command, env, launch_dir, registration_dir,
                                        root / "terminal.txt", barrier)
                    second = pool.submit(launch, second_command, env, launch_dir, registration_dir,
                                         root / "second-terminal.txt", barrier)
                    identity, second_identity = first.result(), second.result()
                expected, second_log = default_log(log_dir, identity), default_log(log_dir, second_identity)
                assert expected != second_log
                assert len(list(log_dir.glob("*.log"))) == 2
                assert_identity(second_log, second_identity, second_workspace)
                assert identity["session_id"] not in second_log.read_text()
                assert second_identity["session_id"] not in expected.read_text()
                assert not (second_workspace / ".nanocodex/logs").exists()
                checks.append({"case": "concurrent-second", **second_identity,
                               "workspace": str(second_workspace), "observed_log": str(second_log),
                               "overlapped_lifetimes": True})
            else:
                identity = launch(command, env, launch_dir, registration_dir, root / "terminal.txt")
                expected = expected or default_log(log_dir, identity)
            assert expected.is_file(), f"{case}: missing {expected}"
            assert_identity(expected, identity, workspace)
            assert not (launch_dir / ".nanocodex/logs").exists(), f"{case}: launch directory polluted"
            assert not (workspace / ".nanocodex/logs").exists(), f"{case}: workspace polluted"
            assert not (launch_dir / "relative-state").exists(), "relative XDG path was used"
            if case == "flag-override":
                assert not (launch_dir / "custom/env.log").exists(), "environment beat CLI flag"
            if case.endswith("override"):
                assert not (home / ".local/state").exists(), "override created a default directory"
            if case in ("xdg", "env-override"):
                before = expected.read_bytes()
                assert before, "startup identity was not flushed to the log"
                reopened = launch(command, env, launch_dir, registration_dir, root / "reopen-terminal.txt")
                if case == "xdg":
                    new_log = default_log(log_dir, reopened)
                    assert new_log != expected, "restart reused the previous default log"
                    assert expected.read_bytes() == before, "restart modified the previous default log"
                    assert_identity(new_log, reopened, workspace)
                else:
                    assert expected.read_bytes().startswith(before), "explicit log was truncated"
                    assert_identity(expected, reopened, workspace)
                checks.append({"case": case + "-reopened", **reopened,
                               "observed_log": str(new_log if case == "xdg" else expected)})
            checks.append({"case": case, "command": shlex.join(command), **identity,
                           "workspace": str(workspace), "observed_log": str(expected),
                           "workspace_logs_created": False})
        # Headless mode does not need a user-state directory. Fail before any
        # model request using incompatible durable-operation flags.
        env.pop("NANOCODEX_LOG_FILE", None)
        env["XDG_STATE_HOME"] = str(root / "blocked-state")
        (root / "blocked-state").write_text("not a directory")
        command = [str(binary), "run", "--request-id", "synthetic-request", "--repeat", "2",
                   "--api-key", "synthetic-test-key", "--browser=none", "--mcp-defaults", "false",
                   "--web-search", "false", "--image-generation", "false", "never submitted"]
        for case, extra in (("headless", []), ("unwritable-log", ["--log-file", str(home)])):
            result = subprocess.run(command + extra, env=env, cwd=launch_dir, capture_output=True,
                                    text=True, timeout=15)
            (artifact / f"{case}.stderr.txt").write_text(result.stderr)
            assert result.returncode != 0, f"{case}: expected a configuration error"
            if case == "headless":
                assert "cannot be combined with `--repeat`" in result.stderr, result.stderr
                assert "tracing output" not in result.stderr, result.stderr
            else:
                assert "failed to open tracing output" in result.stderr, result.stderr
            assert not (launch_dir / ".nanocodex/logs").exists()
            checks.append({"case": case, "command": shlex.join(command + extra),
                           "exit": result.returncode, "stderr": result.stderr})
        outcome["success"] = True
    finally:
        binary.unlink()
        (artifact / "outcome.json").write_text(json.dumps(outcome, indent=2) + "\n")
        print(f"TUI log-location evidence: {artifact}")


if __name__ == "__main__":
    main()
