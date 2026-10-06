#!/usr/bin/env python3
"""Exercise the standalone menu's real CLI installation without stopping the Hand.

Run in the logged-in macOS GUI user's canonical HOME:
  python3 scripts/tests/hand-menu-bar-macos.py --cli /absolute/path/to/nanocodex

Opens/repairs the menu twice and leaves it open. Requires an already running
canonical Hand and refuses a running full Nanocodex Mac app. Never invokes
Start, Stop, Restart, Quit, account login, or Hand enrollment. Process receipts
prove launch/idempotency; a separate GUI observation must verify the visible icon.
Evidence contains only safe status fields, process IDs, paths and hashes, never
complete launchd output, plist contents, account files or process arguments.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import pwd
import re
import stat
import subprocess
import time
from datetime import datetime, timezone


MENU_LABEL = "com.nanocodex.hand-menu-bar"
HELPER_NAME = "nanocodex-hand-menu-bar"


class JourneyFailure(Exception):
    pass


def require(condition, message):
    if not condition:
        raise JourneyFailure(message)


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            result.update(chunk)
    return result.hexdigest()


class Journey:
    def __init__(self, cli, evidence):
        self.cli = cli
        self.evidence = evidence
        self.uid = os.getuid()
        self.home = Path(pwd.getpwuid(self.uid).pw_dir).resolve()
        self.hand_plist = self.home / "Library/LaunchAgents/com.nanocodex.hand.plist"
        self.helper = self.home / ".nanocodex/menu-bar/Nanocodex Hand.app/Contents/MacOS" / HELPER_NAME
        self.target = f"gui/{self.uid}/{MENU_LABEL}"
        # Avoid inherited account/config overrides. All commands act as the
        # actual GUI user against the canonical installation, with no secrets.
        self.env = {"HOME": str(self.home), "USER": pwd.getpwuid(self.uid).pw_name,
                    "PATH": "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
                    "NANOCODEX_DIR": str(self.home / ".nanocodex"), "NO_COLOR": "1"}

    def run(self, args, timeout=20):
        started = time.monotonic()
        receipt = {"argv": [str(arg) for arg in args]}
        self.evidence["commands"].append(receipt)
        try:
            result = subprocess.run(args, env=self.env, stdin=subprocess.DEVNULL,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                    timeout=timeout, check=False)
        except subprocess.TimeoutExpired:
            receipt.update(timed_out=True, elapsed_seconds=round(time.monotonic() - started, 3))
            raise JourneyFailure(f"Command timed out: {Path(args[0]).name}; no automatic retry") from None
        receipt.update(returncode=result.returncode,
                       elapsed_seconds=round(time.monotonic() - started, 3),
                       stdout_sha256=hashlib.sha256(result.stdout).hexdigest(),
                       stderr_sha256=hashlib.sha256(result.stderr).hexdigest())
        return result

    def processes(self):
        result = self.run(["/bin/ps", "-ww", "-axo", "pid=,uid=,comm="])
        require(result.returncode == 0, "Cannot inspect executable process names")
        processes = []
        for line in result.stdout.decode("utf-8", "replace").splitlines():
            match = re.fullmatch(r"\s*(\d+)\s+(-?\d+)\s+(.+)", line)
            require(match is not None, "Unrecognized ps executable record")
            processes.append((int(match[1]), int(match[2]), match[3]))
        return processes

    def no_full_app(self, processes):
        require(not any(Path(name).name == "Nanocodex" for _, _, name in processes),
                "Full Nanocodex Mac app is running; quit it separately before this journey")

    def hand_snapshot(self):
        require(stat.S_ISREG(self.hand_plist.lstat().st_mode), "Canonical Hand plist must be a regular file")
        before_hash = digest(self.hand_plist)
        result = self.run([str(self.cli), "hand", "status"])
        require(result.returncode == 0, "Canonical hand status failed; raw output intentionally omitted")
        try:
            status = json.loads(result.stdout)
        except (ValueError, UnicodeDecodeError):
            raise JourneyFailure("Canonical hand status was not JSON") from None
        require(type(status) is dict, "Canonical hand status must be an object")
        safe = {key: status.get(key) for key in ("installed", "loaded", "pid", "executable")}
        require(safe["installed"] is True and safe["loaded"] is True
                and type(safe["pid"]) is int and safe["pid"] > 0,
                "An already installed, loaded, live canonical Hand is required")
        require(isinstance(safe["executable"], str) and safe["executable"].startswith("/"),
                "Canonical Hand executable receipt is missing")
        os.kill(safe["pid"], 0)
        require(digest(self.hand_plist) == before_hash, "Canonical Hand plist changed during observation")
        return {"status": safe, "plist_sha256": before_hash}

    def helper_snapshot(self):
        result = self.run(["/bin/launchctl", "print", self.target])
        if result.returncode == 113:
            return None
        require(result.returncode == 0, "Cannot inspect menu LaunchAgent")
        output = result.stdout.decode("utf-8", "replace")
        pid_match = re.search(r"^\s*pid = (\d+)\s*$", output, re.MULTILINE)
        program_match = re.search(r"^\s*program = (.+)$", output, re.MULTILINE)
        if not pid_match:
            return None
        require(program_match is not None and Path(program_match[1].strip()) == self.helper,
                "Loaded menu executable is not the canonical standalone helper")
        pid = int(pid_match[1])
        processes = self.processes()
        self.no_full_app(processes)
        matches = [(process_pid, uid, name) for process_pid, uid, name in processes
                   if uid == self.uid and Path(name).name == HELPER_NAME]
        require(len(matches) == 1, "Expected exactly one standalone menu executable process")
        require(matches[0][0] == pid and matches[0][1] == self.uid
                and Path(matches[0][2]) == self.helper,
                "Menu process does not match the canonical launchd PID, user and executable")
        os.kill(pid, 0)
        return {"pid": pid, "executable": str(self.helper), "matching_process_count": len(matches)}

    def wait_helper(self):
        deadline = time.monotonic() + 15
        while True:
            snapshot = self.helper_snapshot()
            if snapshot is not None:
                return snapshot
            require(time.monotonic() < deadline, "Menu did not produce a live process within 15 seconds")
            time.sleep(0.25)

    def exercise(self):
        require(self.uid != 0, "Run as the logged-in GUI user, without sudo")
        require(Path(os.environ.get("HOME", "")).resolve() == self.home,
                "Refusing overridden HOME: a private HOME does not isolate launchd labels")
        require(os.stat("/dev/console").st_uid == self.uid, "Current user does not own the macOS GUI console")
        require(self.run(["/bin/launchctl", "print", f"gui/{self.uid}"]).returncode == 0,
                "No accessible macOS GUI launchd domain")
        self.no_full_app(self.processes())
        self.evidence["full_mac_app_absent"] = True
        baseline = self.hand_snapshot()
        self.evidence["hand_before"] = baseline
        try:
            for iteration in (1, 2):
                self.no_full_app(self.processes())
                result = self.run([str(self.cli), "hand", "menu-bar"], timeout=60)
                require(result.returncode == 0, "hand menu-bar failed; no automatic retry")
                current = self.wait_helper()
                # An immediate PID can be a crashing launch. Observe again,
                # and require repeat invocation to preserve the same PID.
                time.sleep(2)
                require(self.helper_snapshot() == current, "Menu process did not remain stable")
                self.evidence[f"menu_after_{iteration}"] = current
                hand = self.hand_snapshot()
                self.evidence[f"hand_after_{iteration}"] = hand
                require(hand == baseline, "Canonical Hand PID, executable or plist changed")
            require(self.evidence["menu_after_1"] == self.evidence["menu_after_2"],
                    "Second menu-bar invocation restarted or duplicated the unchanged helper")
        finally:
            # Even a menu failure receives a read-only live-Hand check. Do not
            # attempt repair, rollback, service controls, or fixture teardown.
            try:
                after = self.hand_snapshot()
                self.evidence["hand_final"] = after
                self.evidence["canonical_hand_preserved"] = after == baseline
            except (JourneyFailure, OSError) as error:
                self.evidence["final_observation_error"] = type(error).__name__
                self.evidence["canonical_hand_preserved"] = False
        require(self.evidence["canonical_hand_preserved"], "Final canonical Hand preservation check failed")
        self.no_full_app(self.processes())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", required=True, type=Path, help="Absolute path to the real candidate CLI")
    parser.add_argument("--evidence-dir", type=Path, help="Directory for this run's safe JSON evidence")
    args = parser.parse_args()
    if platform.system() != "Darwin":
        parser.error("macOS is required")
    if not args.cli.is_absolute() or not args.cli.is_file() or not os.access(args.cli, os.X_OK):
        parser.error("--cli must be an absolute executable file")
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
    root = Path(__file__).resolve().parents[2]
    directory = args.evidence_dir or root / "output/hand-menu-bar-macos" / stamp
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    evidence_path = directory / f"transcript-{stamp}.json"
    evidence = {"started_at": stamp, "result": "running", "cli": str(args.cli),
                "cli_sha256": digest(args.cli), "commands": [],
                "scope": "real CLI and process receipts; GUI visibility requires separate observation",
                "service_controls_invoked": False, "menu_left_open": True}
    exit_code = 1
    try:
        Journey(args.cli, evidence).exercise()
        evidence["result"] = "passed"
        exit_code = 0
    except JourneyFailure as error:
        evidence.update(result="failed", error=str(error))
    except Exception as error:
        # Never serialize an arbitrary exception containing captured output.
        evidence.update(result="failed", error=f"Unexpected {type(error).__name__}; raw details omitted")
    finally:
        evidence["finished_at"] = datetime.now(timezone.utc).isoformat()
        with evidence_path.open("x", encoding="utf-8") as destination:
            os.chmod(evidence_path, 0o600)
            json.dump(evidence, destination, indent=2)
            destination.write("\n")
        print(json.dumps({"result": evidence["result"], "evidence": str(evidence_path),
                          "error": evidence.get("error")}))
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
