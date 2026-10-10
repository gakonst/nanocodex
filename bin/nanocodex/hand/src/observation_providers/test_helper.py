"""Boundary tests runnable without GI or a desktop: python3 -m unittest discover -s ..."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import time
import unittest

spec = importlib.util.spec_from_file_location("helper", Path(__file__).with_name("helper.py"))
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)


class SnapshotTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name) / "snapshot.json"
        self.context = {"app": "Example", "window": "Main"}
        self.snapshot = {"schemaVersion": 1, "capturedAt": int(time.time() * 1000),
                         **self.context, "data": {"label": "hello"}}

    def write(self):
        self.path.write_text(json.dumps(self.snapshot))

    def read(self):
        self.write()
        return helper.external(self.path, self.context)

    def test_valid_snapshot_and_useful_structural_budget(self):
        self.snapshot["data"] = {"elements": [{"text": str(i), "x": i} for i in range(100)]}
        result = self.read()
        self.assertEqual(result["status"], "ok")
        self.assertEqual(len(result["data"]["elements"]), 100)

    def test_no_context_never_opens_file(self):
        self.assertEqual(helper.external("/not/present", None)["error"], "context_required")

    def test_mismatch_never_returns_data_or_timestamp(self):
        for key in ("app", "window"):
            self.snapshot[key] = "Other"
            result = self.read()
            self.assertEqual(result, {"status": "unavailable", "error": "context_mismatch"})
            self.snapshot[key] = self.context[key]

    def test_schema_and_future_timestamp(self):
        for key, value in [("schemaVersion", 2), ("schemaVersion", True), ("capturedAt", True), ("app", 7), ("window", ""),
                           ("capturedAt", int(time.time() * 1000) + 5000), ("data", [])]:
            old = self.snapshot[key]
            self.snapshot[key] = value
            self.assertEqual(self.read()["error"], "invalid_snapshot")
            self.snapshot[key] = old
        self.snapshot["extra"] = "not allowed"
        self.assertEqual(self.read()["error"], "invalid_snapshot")

    def test_stale_timestamp_preserved_for_registry(self):
        self.snapshot["capturedAt"] -= 10000
        self.assertEqual(self.read()["capturedAt"], self.snapshot["capturedAt"])

    def test_byte_depth_string_and_structural_limits(self):
        deep = {}
        for _ in range(10):
            deep = {"next": deep}
        for data in [{"value": float("nan")}, {"text": "a" * 513}, {"text": "λ" * 257}, deep,
                     {"items": [0] * 2049}, {"items": ["a" * 500] * 20}]:
            self.snapshot["data"] = data
            self.assertEqual(self.read()["error"], "snapshot_limits_exceeded")
        self.snapshot["data"] = {"text": "a" * 65536}
        self.assertEqual(self.read()["error"], "snapshot_too_large")

    @unittest.skipUnless(hasattr(os, "getuid"), "Unix ownership policy")
    def test_other_user_file_rejected_before_read(self):
        from types import SimpleNamespace
        from unittest.mock import patch
        self.write()
        info = SimpleNamespace(st_mode=self.path.stat().st_mode, st_uid=os.getuid() + 1)
        with patch.object(os, "fstat", return_value=info):
            self.assertEqual(helper.external(self.path, self.context)["error"], "unsafe_snapshot_file")

    @unittest.skipUnless(hasattr(os, "O_NOFOLLOW"), "Unix file policy")
    def test_symlink_rejected(self):
        self.write()
        link = self.path.with_name("link")
        link.symlink_to(self.path)
        with self.assertRaises(OSError):
            helper.external(link, self.context)

    @unittest.skipUnless(hasattr(os, "mkfifo"), "Unix FIFO policy")
    def test_fifo_does_not_block(self):
        os.mkfifo(self.path)
        self.assertEqual(helper.external(self.path, self.context)["error"], "unsafe_snapshot_file")

class AtspiTests(unittest.TestCase):
    def test_scoped_tree_bounds_states_and_password_subtree(self):
        from types import SimpleNamespace
        from unittest.mock import patch
        flags = SimpleNamespace(ACTIVE=1, SHOWING=2, ENABLED=3, FOCUSED=4)

        class Node:
            def __init__(self, name, children=(), states=(), password=False):
                self.name, self.children, self.states, self.password = name, children, states, password

            def get_name(self):
                if self.password:
                    raise AssertionError("password name must never be queried")
                return self.name

            def get_child_count(self):
                if self.password:
                    raise AssertionError("password children must never be queried")
                return len(self.children)

            def get_child_at_index(self, index):
                return self.children[index]

            def get_state_set(self):
                return SimpleNamespace(contains=lambda value: value in self.states)

            def get_role(self):
                return 99 if self.password else 1

            def get_role_name(self):
                return "label"

            def get_component_iface(self):
                return SimpleNamespace(get_extents=lambda coord: SimpleNamespace(x=10, y=20, width=30, height=40))

        secret = Node("secret-child")
        password = Node("secret", [secret], password=True)
        visible = Node("λ" * 600, states=[flags.SHOWING, flags.ENABLED, flags.FOCUSED])
        hidden = Node("hidden")
        window = Node("Main", [password, visible, hidden], [flags.ACTIVE, flags.SHOWING])
        desktop = Node("Desktop", [Node("Example", [window]), Node("Unrelated", [Node("do not expose")])])
        fake = SimpleNamespace(StateType=flags, Role=SimpleNamespace(PASSWORD_TEXT=99),
                               CoordType=SimpleNamespace(SCREEN=1), set_timeout=lambda *args: None,
                               get_desktop=lambda index: desktop)
        gi = SimpleNamespace(require_version=lambda *args: None)
        with patch.dict("sys.modules", {"gi": gi, "gi.repository": SimpleNamespace(Atspi=fake)}), patch.dict(os.environ, {"DBUS_SESSION_BUS_ADDRESS": "test"}):
            result = helper.atspi(None)
            self.assertEqual(result["status"], "partial")  # UTF-8 text was truncated.
            nodes = result["data"]["nodes"]
            self.assertEqual(len(nodes), 3)
            self.assertEqual(nodes[1]["bounds"], {"x": 10, "y": 20, "width": 30, "height": 40})
            self.assertEqual(nodes[1]["states"], {"showing": True, "enabled": True, "focused": True})
            self.assertLessEqual(len(nodes[1]["name"].encode()), 512)
            self.assertFalse(nodes[2]["states"]["showing"])
            self.assertNotIn("secret", json.dumps(result))
            self.assertNotIn("Unrelated", json.dumps(result))
            self.assertEqual(helper.atspi({"app": "Other", "window": "Main"})["error"], "matching_window_unavailable")


if __name__ == "__main__":
    unittest.main()
