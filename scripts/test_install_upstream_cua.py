"""Exercise the local-copy launcher without an installed app or native UI."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


class InstalledProviderCopyTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.app = self.root / 'Provider Fixture.app'
        self.source = self.app / 'Contents/Resources/cua_node'
        self.destination = self.root / 'copied provider'
        modules = self.source / 'lib/node_modules/@oai'
        for package in ('cua', 'cua-repl', 'sky'):
            directory = modules / package
            directory.mkdir(parents=True)
            (directory / 'package.json').write_text(json.dumps({'version': '0.0.0-fixture'}))
        (self.source / 'manifest.json').write_text('{"fixture":true}')
        provider = modules / 'cua-repl/bin/cua-repl.mjs'
        provider.parent.mkdir()
        provider.write_text('// unchanged fixture provider\n')
        binary = self.source / 'bin'
        binary.mkdir()
        for name in ('node', 'node_repl'):
            path = binary / name
            path.write_text('#!/bin/sh\nexit 0\n')
            path.chmod(0o755)
        (binary / 'node-link').symlink_to('node')

    def install(self, destination=None, surfaces='computer'):
        return subprocess.run([
            sys.executable, str(Path(__file__).with_name('install-upstream-cua.py')),
            '--source-app', str(self.app), '--destination', str(destination or self.destination),
            '--surfaces', surfaces,
        ], text=True, capture_output=True)

    def test_exact_copy_receipt_and_quoted_launcher(self):
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        receipt = json.loads((self.destination / 'copy-manifest.json').read_text())
        self.assertTrue(receipt['exact_copy'])
        self.assertEqual(receipt['package_versions']['cua-repl'], '0.0.0-fixture')
        self.assertEqual((self.destination / 'cua_node/bin/node-link').readlink(), Path('node'))
        launcher = self.destination / 'cua-provider'
        self.assertIn('CUA_REPL_ENABLED_SURFACES=computer', launcher.read_text())
        self.assertEqual(subprocess.run([str(launcher)], capture_output=True).returncode, 0)
        self.assertEqual(self.install().returncode, 0, 'matching copies can be reused')

    def test_launcher_enables_browser_ax_in_trusted_provider_environment(self):
        node = self.source / 'bin/node'
        node.write_text('#!/bin/sh\nprintf "%s\\n" "$BROWSER_USE_TINYSKY_ENABLED" '
                        '"$CUA_REPL_ENABLED_SURFACES" "$NODE_REPL_UNTRUSTED_ENV_ALLOWLIST"\n')
        for surfaces in ('computer', 'browser', 'browser,computer'):
            with self.subTest(surfaces=surfaces):
                destination = self.root / surfaces
                result = self.install(destination, surfaces)
                self.assertEqual(result.returncode, 0, result.stderr)
                environment = {**os.environ, 'BROWSER_USE_TINYSKY_ENABLED': '0',
                               'NODE_REPL_UNTRUSTED_ENV_ALLOWLIST': ''}
                launched = subprocess.run([str(destination / 'cua-provider')],
                                          env=environment, text=True, capture_output=True)
                self.assertEqual(launched.returncode, 0, launched.stderr)
                self.assertEqual(launched.stdout.splitlines(), ['1', surfaces, ''])

    def test_changed_copy_is_rejected_without_overwrite(self):
        self.assertEqual(self.install().returncode, 0)
        copied = self.destination / 'cua_node/manifest.json'
        copied.write_text('{"changed":true}')
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Copy differs', result.stderr)
        self.assertEqual(copied.read_text(), '{"changed":true}')

    def test_extra_files_and_in_place_destination_are_rejected(self):
        self.assertEqual(self.install().returncode, 0)
        (self.destination / 'cua_node/extra').write_text('unexpected')
        self.assertIn('unexpected files', self.install().stderr)
        self.assertIn('outside the installed source runtime', self.install(self.source).stderr)

    def test_legacy_bundle_is_rejected(self):
        (self.source / 'lib/node_modules/@oai/cua-repl/bin/cua-repl.mjs').unlink()
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('legacy Sky-only runtimes are unsupported', result.stderr)
        self.assertFalse(self.destination.exists())


if __name__ == '__main__':
    unittest.main()
