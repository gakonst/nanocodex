#!/usr/bin/env python3
"""Scoped ARC-AGI-3 interaction client for one normalized eval task."""

import http.cookiejar
import json
import math
import os
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path


CASE_PATH = Path("/opt/nanocodex/arc-agi-3/case.json")
STATE_PATH = Path("/tmp/nanocodex-arc-agi-3-state.json")
COOKIE_PATH = Path("/tmp/nanocodex-arc-agi-3-cookies.txt")


class ArcError(RuntimeError):
    pass


class ArcSession:
    def __init__(self):
        self.case = json.loads(CASE_PATH.read_text())
        self.cookies = http.cookiejar.MozillaCookieJar(str(COOKIE_PATH))
        if COOKIE_PATH.exists():
            self.cookies.load(ignore_discard=True, ignore_expires=True)
        self.opener = urllib.request.build_opener(
            urllib.request.HTTPCookieProcessor(self.cookies)
        )

    def observe(self):
        state = self._load_state()
        if state is None:
            state = self._start()
        print(self._render(state))

    def act(self, argv):
        state = self._load_state()
        if state is None:
            raise ArcError("run `arc-agi-3 observe` before taking an action")
        if state["terminal"]:
            print(self._render(state))
            return
        action, payload = self._parse_action(argv, state)
        frame = self._validated_frame(self._request(
            f"/api/cmd/{action}",
            method="POST",
            key=state["api_key"],
            body={"game_id": self.case["game_id"], "guid": state["guid"], **payload},
        ))
        state["actions"] += 1
        state["level_actions"] += 1
        state["previous_action"] = action
        state["guid"] = frame["guid"]
        state["frame"] = frame
        state["observations"] = [frame]
        self._sync_level(state)
        self._update_terminal(state)

        if not state["terminal"] and frame["state"] in ("GAME_OVER", "NOT_PLAYED"):
            frame = self._validated_frame(self._request(
                "/api/cmd/RESET",
                method="POST",
                key=state["api_key"],
                body={"game_id": self.case["game_id"], "guid": state["guid"]},
            ))
            state["actions"] += 1
            state["level_actions"] += 1
            state["forced_resets"] += 1
            state["previous_action"] = "RESET"
            state["guid"] = frame["guid"]
            state["frame"] = frame
            state["observations"].append(frame)
            self._sync_level(state)

        self._update_terminal(state)
        self._save_state(state)
        print(self._render(state))

    def finish(self):
        state = self._load_state()
        if state is None:
            raise ArcError("there is no ARC-AGI-3 session to finish")
        if not state["terminal"]:
            state["terminal"] = True
            state["exit_reason"] = "AGENT_FINISHED"
            self._save_state(state)
        print(self._render(state))

    def _start(self):
        key = self._request("/api/games/anonkey")["api_key"]
        games = self._request("/api/games", key=key)
        metadata = next(
            (game for game in games if game.get("game_id") == self.case["game_id"]),
            None,
        )
        if metadata is None:
            raise ArcError(f"official API does not offer {self.case['game_id']}")
        if metadata.get("baseline_actions") != self.case["baseline_actions"]:
            raise ArcError("official game baseline changed; run eval preparation again")
        scorecard = self._request(
            "/api/scorecard/open",
            method="POST",
            key=key,
            body={"tags": ["nanocodex", "arc-agi-3", "adapter-smoke"]},
        )
        state = {
            "schema_version": 1,
            "api_key": key,
            "card_id": scorecard["card_id"],
            "guid": None,
            "frame": None,
            "observations": [],
            "actions": 0,
            "level_actions": 0,
            "last_levels_completed": 0,
            "level_just_advanced": False,
            "forced_resets": 0,
            "previous_action": None,
            "terminal": False,
            "exit_reason": None,
        }
        # Persist the close capability before starting the game so verifier
        # cleanup can close a card even when RESET fails.
        self._save_state(state)
        frame = self._validated_frame(self._request(
            "/api/cmd/RESET",
            method="POST",
            key=key,
            body={"card_id": scorecard["card_id"], "game_id": self.case["game_id"]},
        ))
        state["guid"] = frame["guid"]
        state["frame"] = frame
        state["observations"] = [frame]
        state["last_levels_completed"] = frame["levels_completed"]
        self._update_terminal(state)
        self._save_state(state)
        return state

    def _request(self, path, method="GET", key=None, body=None):
        headers = {"Accept": "application/json"}
        if key:
            headers["X-API-Key"] = key
        data = None
        if body is not None:
            headers["Content-Type"] = "application/json"
            data = json.dumps(body, separators=(",", ":")).encode()
        request = urllib.request.Request(
            self.case["api_base_url"] + path,
            headers=headers,
            data=data,
            method=method,
        )
        last_error = None
        attempts = 3 if method == "GET" else 1
        for attempt in range(attempts):
            try:
                with self.opener.open(request, timeout=20) as response:
                    document = json.load(response)
                self.cookies.save(ignore_discard=True, ignore_expires=True)
                os.chmod(COOKIE_PATH, 0o600)
                return document
            except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as error:
                last_error = error
                if attempt + 1 < attempts:
                    time.sleep(0.5 * (2**attempt))
        raise ArcError(f"official ARC API request failed for {path}: {last_error}")

    def _parse_action(self, argv, state):
        if not argv:
            raise ArcError("missing action; use ACTION1 through ACTION7")
        action = argv[0].upper()
        available = {f"ACTION{value}" for value in state["frame"]["available_actions"]}
        if state["actions"] > 0 and state["previous_action"] != "RESET":
            available.add("RESET")
        if action not in available:
            raise ArcError(
                f"{action} is unavailable; choose one of {', '.join(sorted(available))}"
            )
        if action == "ACTION6":
            if len(argv) != 3:
                raise ArcError("ACTION6 requires integer X and Y coordinates")
            try:
                x, y = int(argv[1]), int(argv[2])
            except ValueError as error:
                raise ArcError("ACTION6 coordinates must be integers") from error
            if not 0 <= x <= 63 or not 0 <= y <= 63:
                raise ArcError("ACTION6 coordinates must be between 0 and 63")
            return action, {"x": x, "y": y}
        if len(argv) != 1:
            raise ArcError(f"{action} does not accept arguments")
        return action, {}

    def _sync_level(self, state):
        completed = state["frame"]["levels_completed"]
        if completed > state["last_levels_completed"]:
            state["level_actions"] = 0
            state["last_levels_completed"] = completed
            state["level_just_advanced"] = True

    def _update_terminal(self, state):
        frame = state["frame"]
        if frame["state"] == "WIN":
            state["terminal"] = True
            state["exit_reason"] = "GAME_WIN"
            return
        smoke_cap = self.case.get("smoke_action_cap")
        if smoke_cap is not None and state["actions"] >= smoke_cap:
            state["terminal"] = True
            state["exit_reason"] = "SMOKE_ACTION_CAP"
            return
        level = frame["levels_completed"]
        baselines = self.case["baseline_actions"]
        if level < len(baselines):
            budget = math.ceil(
                baselines[level] * self.case["action_budget_multiplier"]
            )
            if state["level_actions"] >= budget:
                state["terminal"] = True
                state["exit_reason"] = "ACTION_BUDGET"
                return
        total_budget = sum(
            math.ceil(value * self.case["action_budget_multiplier"])
            for value in baselines
        )
        if state["actions"] >= total_budget:
            state["terminal"] = True
            state["exit_reason"] = "ACTION_BUDGET"

    def _render(self, state):
        if state["frame"] is None:
            return "Status: SETUP_FAILED\nThe scorecard was opened but no initial frame was received."
        observations = state.get("observations") or [state["frame"]]
        rendered_observations = []
        for observation_index, frame in enumerate(observations):
            rendered_observations.append(
                self._render_frame(
                    frame,
                    state["level_just_advanced"] and observation_index == len(observations) - 1,
                    state,
                )
            )
        state["level_just_advanced"] = False
        status = "TERMINAL" if state["terminal"] else "ACTIVE"
        rendered_observations.append(
            f"Status: {status}\nActions recorded: {state['actions']}\n"
            f"Exit reason: {state['exit_reason'] or 'NONE'}"
        )
        return "\n\n".join(rendered_observations)

    def _render_frame(self, frame, new_level, state):
        grids = self._interpolate(
            frame["frame"], self.case["max_animation_frames"]
        )
        parts = [
            f"State: {frame['state']}\nLevels completed: {frame['levels_completed']}"
        ]
        for index, grid in enumerate(grids):
            lines = [f"Frame {index}:"]
            if new_level and index == len(grids) - 1:
                lines = ["New Level:", "", *lines]
            lines.extend(f"  {row}" for row in grid)
            parts.append("\n".join(lines))
        actions = [f"ACTION{value}" for value in frame["available_actions"]]
        if state["actions"] > 0 and state["previous_action"] != "RESET":
            actions.insert(0, "RESET")
        rendered = []
        for action in actions:
            if action == "ACTION6":
                rendered.append("- ACTION6 x y  (where x and y are integers 0-63)")
            else:
                rendered.append(f"- {action}")
        parts.append("Available actions:\n" + "\n".join(rendered))
        return "\n\n".join(parts)

    def _validated_frame(self, frame):
        if not isinstance(frame, dict):
            raise ArcError("official API returned a non-object frame")
        required = {
            "game_id", "frame", "state", "levels_completed", "win_levels",
            "guid", "available_actions",
        }
        if not required.issubset(frame):
            raise ArcError("official API returned an incomplete frame")
        if frame["game_id"] != self.case["game_id"] or not isinstance(frame["guid"], str):
            raise ArcError("official API returned a frame for the wrong game or session")
        if frame["state"] not in {"NOT_PLAYED", "NOT_FINISHED", "WIN", "GAME_OVER"}:
            raise ArcError(f"official API returned unknown state {frame['state']!r}")
        if type(frame["levels_completed"]) is not int or type(frame["win_levels"]) is not int:
            raise ArcError("official API returned invalid level counters")
        actions = frame["available_actions"]
        if not isinstance(actions, list) or any(type(value) is not int or not 1 <= value <= 7 for value in actions):
            raise ArcError("official API returned invalid available actions")
        if not isinstance(frame["frame"], list) or not frame["frame"]:
            raise ArcError("official API returned no rendered frame")
        for grid in frame["frame"]:
            if not isinstance(grid, list) or any(not isinstance(row, list) for row in grid):
                raise ArcError("official API returned an invalid rendered frame")
        return frame

    @staticmethod
    def _interpolate(frames, target):
        if len(frames) <= target:
            return frames
        if target == 1:
            return [frames[-1]]
        indexes = [round(index * (len(frames) - 1) / (target - 1)) for index in range(target)]
        return [frames[index] for index in indexes]

    @staticmethod
    def _load_state():
        if not STATE_PATH.exists():
            return None
        state = json.loads(STATE_PATH.read_text())
        if state.get("schema_version") != 1:
            raise ArcError("unsupported private session state")
        return state

    @staticmethod
    def _save_state(state):
        descriptor, name = tempfile.mkstemp(dir=STATE_PATH.parent, prefix=".arc-state-")
        try:
            with os.fdopen(descriptor, "w") as output:
                json.dump(state, output, separators=(",", ":"))
                output.flush()
                os.fsync(output.fileno())
            os.chmod(name, 0o600)
            os.replace(name, STATE_PATH)
        finally:
            if os.path.exists(name):
                os.unlink(name)


def self_test():
    session = object.__new__(ArcSession)
    session.case = {
        "api_base_url": "https://invalid.example",
        "game_id": "ls20-test",
        "max_animation_frames": 3,
    }
    state = {
        "frame": {"available_actions": [1, 6]},
        "actions": 0,
        "previous_action": None,
    }
    assert session._parse_action(["ACTION1"], state) == ("ACTION1", {})
    assert session._parse_action(["ACTION6", "2", "63"], state) == (
        "ACTION6",
        {"x": 2, "y": 63},
    )
    try:
        session._parse_action(["ACTION6", "64", "0"], state)
    except ArcError:
        pass
    else:
        raise AssertionError("out-of-range coordinates were accepted")
    assert session._interpolate([[0], [1], [2], [3], [4]], 3) == [[0], [2], [4]]

    frame = {
        "game_id": "ls20-test",
        "frame": [[[0, 1], [1, 0]]],
        "state": "NOT_FINISHED",
        "levels_completed": 0,
        "win_levels": 1,
        "guid": "guid",
        "available_actions": [1, 6],
    }
    assert session._validated_frame(frame) is frame
    malformed = dict(frame, available_actions=[8])
    try:
        session._validated_frame(malformed)
    except ArcError:
        pass
    else:
        raise AssertionError("invalid action metadata was accepted")

    class FailingOpener:
        def __init__(self):
            self.calls = 0

        def open(self, request, timeout):
            self.calls += 1
            raise urllib.error.URLError("lost response")

    session.opener = FailingOpener()
    try:
        session._request("/api/cmd/ACTION1", method="POST", body={})
    except ArcError:
        pass
    else:
        raise AssertionError("failed POST unexpectedly succeeded")
    assert session.opener.calls == 1
    print("ok")


def main():
    if len(sys.argv) == 2 and sys.argv[1] == "self-test":
        self_test()
        return
    command = sys.argv[1] if len(sys.argv) > 1 else ""
    session = ArcSession()
    if command == "observe" and len(sys.argv) == 2:
        session.observe()
    elif command == "act":
        session.act(sys.argv[2:])
    elif command == "finish" and len(sys.argv) == 2:
        session.finish()
    else:
        raise ArcError("usage: arc-agi-3 observe | act ACTION [X Y] | finish")


if __name__ == "__main__":
    try:
        main()
    except ArcError as error:
        print(f"ARC-AGI-3 error: {error}", file=sys.stderr)
        raise SystemExit(2)
