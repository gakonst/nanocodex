import importlib.util
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('catalog_cache', ROOT / 'scripts/cache-catalog.py')
cache = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cache)


class CatalogCacheTests(unittest.TestCase):
    @unittest.skipUnless(shutil.which('lua'), 'Lua required')
    def test_account_text_is_data_and_roundtrips_exactly(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / 'Interface/AddOns/Nanocodex'
            target.mkdir(parents=True)
            snapshot = 'ncw1\nP\tp\tQuotes " ]=] \\ Ω\nT\tp\tt\tend; error("injected")\tidle'
            revision = cache.write_catalog(root, snapshot)
            code = 'local n={};assert(loadfile(arg[1]))("Nanocodex",n);assert(n.CatalogCache.revision==arg[2]);io.write(n.CatalogCache.snapshot)'
            fixture = root / 'check.lua'
            fixture.write_text(code)
            result = subprocess.run(['lua', str(fixture), str(target / 'Catalog.lua'), revision], capture_output=True, check=True)
            self.assertEqual(result.stdout, snapshot.encode())

    def test_rejects_unbounded_or_missing_snapshot_without_replacing_file(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / 'Interface/AddOns/Nanocodex'
            target.mkdir(parents=True)
            file = target / 'Catalog.lua'
            file.write_text('existing')
            for snapshot in ['invalid', 'ncw1\n' + 'x' * (1024 * 1024)]:
                with self.assertRaises(ValueError):
                    cache.write_catalog(root, snapshot)
                self.assertEqual(file.read_text(), 'existing')
