"""Verify the actual release artifact and fail-before-install boundary."""
import importlib.util
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('addon_package', ROOT / 'scripts/package-addon.py')
packager = importlib.util.module_from_spec(spec)
spec.loader.exec_module(packager)


class AddonPackageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='ncw package ')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def test_release_contains_every_manifest_dependency_and_runs_existing_fixtures(self):
        first, second = self.root / 'first.zip', self.root / 'second.zip'
        packager.package(first)
        packager.package(second)
        self.assertEqual(first.read_bytes(), second.read_bytes())
        with zipfile.ZipFile(first) as archive:
            names = archive.namelist()
            self.assertTrue(all(name.startswith('Nanocodex/') for name in names))
            self.assertIn('Nanocodex/Transport.lua', names)
            self.assertIn('Nanocodex/Bindings.xml', names)
            archive.extractall(self.root / 'addon')
        addon = self.root / 'addon/Nanocodex'
        self.assertEqual((addon / 'Transport.lua').read_bytes(), (ROOT / 'addon/Transport.lua').read_bytes())
        self.assertEqual(len(list(addon.glob('*.toc'))), 6)
        for toc in addon.glob('*.toc'):
            entries = [line.strip() for line in toc.read_text().splitlines()
                       if line.strip() and not line.startswith('#')]
            for entry in entries:
                self.assertTrue((addon / entry).is_file(), (toc.name, entry))
            self.assertLess(entries.index('Bridge.lua'), entries.index('Client.lua'))
        for source in addon.glob('*.lua'):
            subprocess.run(['luac', '-p', str(source)], check=True, capture_output=True)
        # Fixtures historically load the carrier from its development path.
        # Alias only the extracted module; never reach back into source.
        (self.root / 'addon/Transport.lua').symlink_to('Nanocodex/Transport.lua')
        # Existing interaction checks execute the extracted files, not source files.
        for fixture in ('ui_test.lua', 'client_test.lua', 'streaming_test.lua'):
            result = subprocess.run(['lua', str(ROOT / 'addon/tests' / fixture)],
                                    cwd=self.root, text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_missing_carrier_preserves_artifact_and_installed_addon(self):
        source = self.root / 'source'
        shutil.copytree(ROOT / 'addon', source / 'addon')
        (source / 'scripts').mkdir()
        for script in ('install-addon.sh', 'package-addon.py'):
            shutil.copyfile(ROOT / 'scripts' / script, source / 'scripts' / script)
        (source / 'addon/Transport.lua').unlink()
        artifact = self.root / 'existing.zip'
        artifact.write_bytes(b'previous release')
        with self.assertRaises(ValueError):
            packager.package(artifact, source)
        self.assertEqual(artifact.read_bytes(), b'previous release')
        client = self.root / 'WoW client'
        installed = client / 'Interface/AddOns/Nanocodex'
        installed.mkdir(parents=True)
        (installed / 'Core.lua').write_text('previous installation')
        result = subprocess.run(['bash', str(source / 'scripts/install-addon.sh'), str(client)],
                                text=True, capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Transport.lua', result.stderr)
        self.assertEqual(list(installed.iterdir()), [installed / 'Core.lua'])
        self.assertEqual((installed / 'Core.lua').read_text(), 'previous installation')
        self.assertFalse((client / 'Interface/NanocodexBackups').exists())

    def test_case_mismatch_and_traversal_are_rejected(self):
        source = self.root / 'source'
        shutil.copytree(ROOT / 'addon', source / 'addon')
        toc = source / 'addon/Nanocodex/Nanocodex.toc'
        for entry in ('core.lua', '../Transport.lua'):
            toc.write_text('## Title: Fixture\n' + entry + '\n')
            with self.subTest(entry=entry), self.assertRaises(ValueError):
                packager.addon_files(source)


if __name__ == '__main__':
    unittest.main()
