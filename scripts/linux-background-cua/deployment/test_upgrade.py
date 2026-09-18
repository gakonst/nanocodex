"""Run on Linux: python3 -m unittest discover -s scripts/linux-background-cua/deployment -p 'test_*.py'."""
import importlib.util
import json
import os
from pathlib import Path
import runpy
import tempfile
import unittest
from unittest.mock import patch

HERE = Path(__file__).parent
spec = importlib.util.spec_from_file_location('upgrade', HERE / 'upgrade-omarchy.py')
u = importlib.util.module_from_spec(spec)
spec.loader.exec_module(u)


@unittest.skipUnless(os.uname().sysname == 'Linux', 'Deployment boundary tests require Linux')
class UpgradeTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.base = Path(self.tmp.name)
        self.root = self.base / 'installed'
        self.stage = self.base / 'stage'
        self.root.mkdir(mode=0o755)
        self.stage.mkdir()
        self.version = {'commit': 'abcdef', 'version': '0.56.2', 'abiHash': 'exact_abi', 'dirty': False}
        for name in u.FILES | {'nanocodex2', 'sudoers'}:
            for folder in (self.root, self.stage):
                (folder / name).write_text(json.dumps(self.version) if name == 'compositor-version.json'
                                          else ('old-' if folder == self.root else 'new-') + name)
                (folder / name).chmod(0o440 if name == 'sudoers' else 0o755)
        self.binary = self.base / 'Hyprland'
        self.binary.write_bytes(b'exact compositor executable')
        self.manifest = self.base / 'manifest.json'
        self.manifest.write_text(json.dumps({'schema': 1, 'compositor': self.version,
            'hyprland_sha256': u.sha(self.binary),
            'files': {name: u.sha(self.stage / name) for name in u.FILES}}))
        self.digest = u.sha(self.manifest)
        self.before = u.inventory(self.root)

    def validate(self):
        return u.validate(self.stage, self.manifest, self.digest, self.root, self.binary)

    def install(self):
        with patch.object(u, 'no_compositor'):
            u.install(self.stage, self.manifest, self.digest, self.root, self.binary)
        return next(self.base.glob('.background-cua-upgrade-*'))

    def test_manifest_and_artifact_tampering_refused(self):
        self.validate()
        (self.stage / 'nanocodex-computer').write_text('tampered')
        with self.assertRaisesRegex(RuntimeError, 'Artifact hash mismatch'):
            self.validate()
        self.assertEqual(u.inventory(self.root), self.before)
        with self.assertRaisesRegex(RuntimeError, 'Manifest SHA-256'):
            u.validate(self.stage, self.manifest, '0' * 64, self.root, self.binary)

    def test_changed_binary_and_installed_abi_refused(self):
        self.binary.write_text('updated')
        with self.assertRaisesRegex(RuntimeError, 'binary changed'):
            self.validate()
        self.binary.write_bytes(b'exact compositor executable')
        (self.root / 'compositor-version.json').write_text(json.dumps({**self.version, 'abiHash': 'other'}))
        with self.assertRaisesRegex(RuntimeError, 'Installed ABI differs'):
            self.validate()

    def test_symlink_artifact_refused(self):
        artifact = self.stage / 'nanocodex-computer'
        artifact.unlink()
        artifact.symlink_to(self.binary)
        with self.assertRaisesRegex(RuntimeError, 'Not a regular file'):
            self.validate()

    def test_active_compositor_refused(self):
        proc = self.base / 'proc'
        (proc / '17646').mkdir(parents=True)
        (proc / '17646' / 'comm').write_text('Hyprland\n')
        with self.assertRaisesRegex(RuntimeError, '17646'):
            u.no_compositor(proc)
        with patch.object(u, 'no_compositor', side_effect=RuntimeError('active')):
            with self.assertRaisesRegex(RuntimeError, 'active'):
                u.install(self.stage, self.manifest, self.digest, self.root, self.binary)
        self.assertFalse(list(self.base.glob('.background-cua-upgrade-*')))
        self.assertEqual(u.inventory(self.root), self.before)

    def test_compositor_appearing_during_prepare_prevents_exchange(self):
        with patch.object(u, 'no_compositor', side_effect=[None, RuntimeError('new compositor')]), \
             patch.object(u, 'exchange') as swap:
            with self.assertRaisesRegex(RuntimeError, 'new compositor'):
                u.install(self.stage, self.manifest, self.digest, self.root, self.binary)
        swap.assert_not_called()
        self.assertEqual(u.inventory(self.root), self.before)

    def test_copy_failure_leaves_installation_untouched(self):
        with patch.object(u.shutil, 'copyfile', side_effect=OSError('injected full disk')):
            with self.assertRaisesRegex(OSError, 'full disk'):
                self.install()
        self.assertEqual(u.inventory(self.root), self.before)

    @unittest.skipUnless(os.uname().sysname == 'Linux', 'Linux atomic exchange')
    def test_round_trip_preserves_unreplaced_files_metadata_and_dry_run(self):
        slot = self.install()
        self.assertEqual(u.inventory(slot / 'previous'), self.before)
        for name in ('nanocodex2', 'sudoers'):
            self.assertEqual(u.inventory(self.root)[name], self.before[name])
        installed = u.inventory(self.root)
        with patch.object(u, 'no_compositor'):
            u.rollback(slot, self.root, self.binary, dry_run=True)
            self.assertEqual(u.inventory(self.root), installed)
            u.rollback(slot, self.root, self.binary)
            self.assertEqual(u.inventory(self.root), self.before)
            u.rollback(slot, self.root, self.binary)  # Idempotent; never toggles back.
        self.assertEqual(u.inventory(slot / 'previous'), installed)

    @unittest.skipUnless(os.uname().sysname == 'Linux', 'Linux atomic exchange')
    def test_interruption_after_exchange_recovered_from_receipt(self):
        real_exchange = u.exchange
        def interrupted(a, b):
            real_exchange(a, b)
            raise OSError('injected interruption after exchange')
        with patch.object(u, 'exchange', side_effect=interrupted):
            with self.assertRaisesRegex(OSError, 'interruption'):
                self.install()
        slot = next(self.base.glob('.background-cua-upgrade-*'))
        with patch.object(u, 'no_compositor'):
            u.rollback(slot, self.root, self.binary)
        self.assertEqual(u.inventory(self.root), self.before)

    def test_interruption_before_exchange_retains_original(self):
        with patch.object(u, 'exchange', side_effect=OSError('injected unsupported filesystem')):
            with self.assertRaisesRegex(OSError, 'unsupported filesystem'):
                self.install()
        slot = next(self.base.glob('.background-cua-upgrade-*'))
        with patch.object(u, 'no_compositor'):
            u.rollback(slot, self.root, self.binary)
        self.assertEqual(u.inventory(self.root), self.before)

    @unittest.skipUnless(os.uname().sysname == 'Linux', 'Linux atomic exchange')
    def test_changed_backup_refuses_rollback(self):
        slot = self.install()
        (slot / 'previous' / 'nanocodex2').write_text('unexpected edit')
        with patch.object(u, 'no_compositor'):
            with self.assertRaisesRegex(RuntimeError, 'differs from receipt'):
                u.rollback(slot, self.root, self.binary)

    def test_new_session_abi_mismatch_refused_before_plugin_command(self):
        # Run the actual activator: commit/version match, but runtime ABI differs.
        replies = [json.dumps([{'instance': 'fresh', 'pid': 123}]).encode(),
                   json.dumps({**self.version, 'abiHash': 'different'}).encode()]
        with patch('os.geteuid', return_value=1000), \
             patch('pathlib.Path.read_text', return_value=json.dumps(self.version)), \
             patch('subprocess.check_output', side_effect=replies) as ctl:
            with self.assertRaisesRegex(SystemExit, 'Compositor differs'):
                runpy.run_path(str(HERE / 'activate-plugin.py'), run_name='__main__')
        self.assertEqual(ctl.call_count, 2)


if __name__ == '__main__':
    unittest.main()
