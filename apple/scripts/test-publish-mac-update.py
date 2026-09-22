#!/usr/bin/env python3
import importlib.util
import json
from pathlib import Path
import plistlib
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('ota', Path(__file__).with_name('publish-mac-update.py'))
ota = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ota)


class PublicationTests(unittest.TestCase):
    def test_manifest_contract(self):
        item = plistlib.loads(ota.manifest('2.0 & beta', '123'))['items'][0]
        self.assertEqual(item['metadata']['bundle-identifier'], ota.BUNDLE)
        self.assertEqual(item['metadata']['bundle-version'], '123')
        self.assertEqual(item['assets'][0]['url'], ota.ORIGIN + '/builds/123/Nanocodex.ipa')

    def test_numeric_build_policy(self):
        for invalid in ['1.2', '01', '../42', '-3', '0', '']:
            with self.assertRaises(ValueError):
                ota.build_number(invalid)
        with self.assertRaises(ValueError):
            ota.policy({'build': '100'}, '99', None, 'new')
        ota.policy({'build': '99'}, '100', None, 'new')

    def test_immutable_policy(self):
        with self.assertRaises(ValueError):
            ota.policy({'build': '100'}, '100', 'old', 'new')
        ota.policy({'build': '100'}, '100', 'same', 'same')

    def test_page_escaping(self):
        page = ota.page('<script>bad</script>', '123')
        self.assertNotIn('<script>', page)
        self.assertIn('&lt;script&gt;', page)
        self.assertIn('itms-services://?action=download-manifest&amp;url=', page)

    def test_preserve_builds_and_latest_on_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            assets = root / 'assets'
            assets.mkdir()
            ipa = root / 'test.ipa'
            ipa.write_bytes(b'first')
            ota.prepare(ipa, assets, '1.0', '100', None)
            original = (assets / 'builds/100/Nanocodex.ipa').read_bytes()
            ipa.write_bytes(b'second')
            with self.assertRaises(ValueError):
                ota.prepare(ipa, assets, '1.0', '100', None)
            self.assertEqual((assets / 'builds/100/Nanocodex.ipa').read_bytes(), original)
            ota.prepare(ipa, assets, '1.1', '101', 'New version')
            latest = (assets / 'latest.json').read_bytes()
            with self.assertRaises(ValueError):
                ota.prepare(ipa, assets, '1.0', '99', None)
            self.assertEqual((assets / 'latest.json').read_bytes(), latest)
            self.assertEqual(json.loads(latest)['build'], '101')
            self.assertTrue((assets / 'builds/100/manifest.plist').exists())


if __name__ == '__main__':
    unittest.main()
