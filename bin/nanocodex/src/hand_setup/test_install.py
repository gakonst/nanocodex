"""Policy tests for the actual installer, without mutating the host."""
import contextlib
import hashlib
import importlib.util
import io
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("hand_install", Path(__file__).with_name("install.py"))
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)


class InstallerTests(unittest.TestCase):
    def test_account_check_uses_identified_client_and_auth_header(self):
        with patch.object(installer.urllib.request, "urlopen", return_value=contextlib.closing(io.BytesIO(b'{"data": []}'))) as request:
            self.assertEqual(installer.account_get({"origin": "https://example.invalid", "credential": "synthetic"}, "/v1/account/hands"), {"data": []})
        sent = request.call_args.args[0]
        self.assertEqual(sent.get_header("User-agent"), "nanocodex-hand-setup")
        self.assertEqual(sent.get_header("Authorization"), "Bearer synthetic")
        self.assertNotIn("synthetic", sent.full_url)

    def test_bad_download_never_replaces_a_cached_artifact(self):
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            expected = hashlib.sha256(b"expected").hexdigest()
            with patch.object(installer.urllib.request, "urlopen", return_value=contextlib.closing(io.BytesIO(b"wrong"))):
                with self.assertRaisesRegex(RuntimeError, "checksum mismatch"):
                    installer.download("https://example.invalid/asset", expected, cache)
            self.assertEqual(list(cache.iterdir()), [])

    def test_verified_cached_artifact_avoids_network(self):
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            expected = hashlib.sha256(b"expected").hexdigest()
            (cache / expected).write_bytes(b"expected")
            with patch.object(installer.urllib.request, "urlopen", side_effect=AssertionError("network")):
                self.assertEqual(installer.download("https://example.invalid/asset", expected, cache), cache / expected)

    def test_credentials_stay_private_on_unchanged_reruns_and_symlinks_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "account.env"
            installer.atomic(path, b"synthetic-secret", 0o600)
            os.chmod(path, 0o644)
            self.assertFalse(installer.atomic(path, b"synthetic-secret", 0o600))
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            linked = Path(directory) / "linked"
            linked.symlink_to(path)
            with self.assertRaisesRegex(RuntimeError, "unexpected file"):
                installer.atomic(linked, b"replacement")
            self.assertEqual(path.read_bytes(), b"synthetic-secret")

    def test_service_waits_for_application_readiness_and_keeps_credentials_out_of_argv(self):
        service = installer.unit("native-hand --workspace /srv/nanocodex/workspace").decode()
        self.assertIn("Type=notify\n", service)
        self.assertIn("NotifyAccess=main\n", service)
        self.assertIn("EnvironmentFile=/opt/nanocodex/account.env\n", service)
        self.assertNotIn("NANOCODEX_API_KEY=", service)


if __name__ == "__main__":
    unittest.main()
