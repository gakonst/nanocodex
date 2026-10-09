#!/usr/bin/env python3
"""Exercise Codex-compatible resume discovery through the shipped CLI's real PTY.

python3 scripts/tests/codex-resume-picker-journey.py --binary target/debug/nanocodex
Synthetic homes and terminal transcripts are retained in ignored output/.
"""
import argparse
import errno
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import select
import shlex
import struct
import subprocess
import termios
import time
from uuid import uuid4


def row(kind, **payload):
    return {"timestamp": "2026-10-06T12:00:00Z", "type": kind, "payload": payload}


def message(role, text):
    return row("response_item", type="message", role=role,
               content=[{"type": "input_text", "text": text}])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", type=Path, default=Path("output/codex-resume-picker") / uuid4().hex)
    args = parser.parse_args()
    binary, artifact = args.binary.resolve(), args.output.resolve()
    artifact.mkdir(parents=True)
    command = [str(binary), "resume"]
    checks = []
    cases = [
        ("codex-order", "Codex prompt after setup", False),
        ("named", "Latest saved session name", True),
        ("native-order", "Nanocodex original prompt", False),
        ("unnamed", "(prompt unavailable)", False),
        ("broken-index", "Prompt survives broken title index", False),
        ("missing-index", "Prompt without title index", False),
        ("blank-name", "Prompt after clearing a saved name", False),
        ("long-name", "界" * 80 + "x" * 80, False),
    ]
    outcome = {"success": False, "command": shlex.join(command), "checks": checks,
               "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest()}
    try:
        for name, expected, archived in cases:
            home = artifact / name
            home.mkdir()
            thread_id = str(uuid4())
            directory = home / ("archived_sessions" if archived else "sessions/2026/10/06")
            directory.mkdir(parents=True)
            path = directory / f"rollout-2026-10-06T12-00-00-{thread_id}.jsonl"
            meta = row("session_meta", id=thread_id, cwd=str(home), history_mode="legacy")
            prompt = "Underlying first prompt" if name in ("named", "long-name") else expected
            event = row("event_msg", type="user_message", message=prompt)
            setup = [message("developer", "Developer setup must not be the preview"),
                     message("user", "# AGENTS.md instructions for /synthetic"),
                     message("user", "<environment_context>synthetic context</environment_context>")]
            if name == "native-order":
                rows = [meta, event, message("user", expected)]
            elif name == "unnamed":
                rows = [meta, *setup, row("event_msg", type="turn_aborted")]
            else:
                rows = [meta, *setup, {"type": "event_msg"}, message("user", prompt), event]
            path.write_text("\n".join(map(json.dumps, rows)) + "\n")
            index = home / "session_index.jsonl"
            if name in ("named", "blank-name", "long-name"):
                saved_name = "Latest saved\nsession \x07name" if name == "named" else "   " if name == "blank-name" else expected + "OVERFLOW"
                entries = [{"id": thread_id, "thread_name": title, "updated_at": timestamp}
                           for title, timestamp in [("Old session name", "2026-10-06T11:00:00Z"),
                                                    (saved_name, "2026-10-06T12:00:00Z")]]
                index.write_text(json.dumps(entries[0]) + "\nnot json\n" + json.dumps(entries[1]) + "\n{partial")
            elif name == "broken-index":
                index.write_text('not json\n{}\n{"id": 4, "thread_name": []}\n{partial')
            elif name != "missing-index":
                index.write_text("")
            if name == "codex-order":
                # Saved names cannot make invalid or incomplete rollouts eligible.
                invalid = []
                for kind in ("metadata-only", "wrong-id", "unsupported-history"):
                    bad_id = str(uuid4())
                    bad_meta = row("session_meta", id=str(uuid4()) if kind == "wrong-id" else bad_id,
                                   cwd=str(home), history_mode="future" if kind == "unsupported-history" else "legacy")
                    bad_rows = [bad_meta] if kind == "metadata-only" else [bad_meta, message("user", "Invalid rollout")]
                    (directory / f"rollout-2026-10-06T12-00-00-{bad_id}.jsonl").write_text("\n".join(map(json.dumps, bad_rows)) + "\n")
                    invalid.append({"id": bad_id, "thread_name": "Must not be listed", "updated_at": "2026-10-06T12:00:00Z"})
                index.write_text("\n".join(map(json.dumps, invalid)) + "\n")
            before = {p: hashlib.sha256(p.read_bytes()).hexdigest() for p in home.rglob("*.jsonl")}
            env = {"HOME": str(home), "CODEX_HOME": str(home), "TERM": "xterm-256color",
                   "PATH": os.environ.get("PATH", "/usr/bin:/bin"), "NANOCODEX_COMPUTER": "off"}
            master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 320, 0, 0))
            child = subprocess.Popen(command, cwd=home, env=env, stdin=slave, stdout=slave,
                                     stderr=slave, start_new_session=True)
            os.close(slave)
            transcript = bytearray()
            deadline = time.monotonic() + 15
            cancelled = False
            try:
                while time.monotonic() < deadline:
                    if select.select([master], [], [], 0.1)[0]:
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
                        if not cancelled and thread_id.encode() in transcript:
                            os.write(master, b"\x1b")
                            cancelled = True
                    if child.poll() is not None:
                        break
                child.wait(timeout=3)
                plain = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", bytes(transcript)).decode(errors="replace")
                assert child.returncode == 0, f"{name}: exit {child.returncode}"
                assert cancelled, f"{name}: session never appeared"
                assert "1 resumable sessions" in plain, f"{name}: unexpected session eligibility"
                assert expected in plain, f"{name}: missing {expected!r}; see {artifact / (name + '.terminal.txt')}"
                if name != "unnamed":
                    assert "(prompt unavailable)" not in plain, f"{name}: missing preview"
                assert "Developer setup must" not in plain and "# AGENTS.md" not in plain and "<environment_context>" not in plain
                assert "Must not be listed" not in plain and "Old session name" not in plain
                assert "OVERFLOW" not in plain
                if name in ("named", "long-name"):
                    assert "Underlying first prompt" not in plain
                if archived:
                    assert "archived" in plain
                assert before == {p: hashlib.sha256(p.read_bytes()).hexdigest() for p in home.rglob("*.jsonl")}, "picker mutated session store"
                checks.append({"case": name, "expected": expected, "observed": expected, "cancel_exit": child.returncode, "store_unchanged": True})
            finally:
                if child.poll() is None:
                    child.terminate()
                    try:
                        child.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        child.kill()
                        child.wait()
                os.close(master)
                (artifact / f"{name}.pty.log").write_bytes(transcript)
                (artifact / f"{name}.terminal.txt").write_bytes(re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", bytes(transcript)))
        outcome["success"] = True
    finally:
        (artifact / "outcome.json").write_text(json.dumps(outcome, indent=2) + "\n")
        print(f"Resume picker evidence: {artifact}")


if __name__ == "__main__":
    main()
