#!/usr/bin/env python3
"""Black-box packaging preparation journeys; does not claim an iOS compile."""
import hashlib
import json
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tempfile

from PIL import Image

ROOT = Path(__file__).resolve().parents[2]
EVIDENCE = ROOT / 'output/ios-linux'
EVIDENCE.mkdir(parents=True, exist_ok=True)
trace = []


def run(apple, *args, ok=True):
    command = [sys.executable, str(apple/'scripts/prepare-xtool.py'), *args]
    result = subprocess.run(command, text=True, capture_output=True)
    trace.append('$ ' + ' '.join(command) + '\n' + result.stdout + result.stderr + f'exit={result.returncode}\n')
    assert (result.returncode == 0) == ok, trace[-1]
    return result


def tree_digest(root):
    return {str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in root.rglob('*') if p.is_file()}


try:
    with tempfile.TemporaryDirectory(prefix='packaging-', dir=EVIDENCE) as temp:
        apple = Path(temp)/'apple'
        shutil.copytree(ROOT/'apple', apple, symlinks=True, ignore=shutil.ignore_patterns('.build', 'Artifacts', 'generated', '*.ipa', '*.app', '.swiftpm'))
        generated = apple/'xtool/generated'
        run(apple, '--version', '1.2.3', '--build-number', '42', '--sdk-version', '26.3')
        assert (generated/'Package.resolved').read_bytes() == (apple/'NanocodexInbox.xcodeproj/project.xcworkspace/xcshareddata/swiftpm/Package.resolved').read_bytes()
        config = json.loads((generated/'xtool.yml').read_text())
        assert config['bundleID'] == 'xyz.paradigm.centaur'
        assert {e['bundleID'] for e in config['extensions']} == {'xyz.paradigm.centaur.share', 'xyz.paradigm.centaur.widgets'}
        for name in ('NanocodexInbox', 'NanocodexShare', 'NanocodexWidgets'):
            info = plistlib.loads((generated/f'{name}.plist').read_bytes())
            assert info['CFBundleVersion'] == '42'
            assert info['CFBundleShortVersionString'] == '1.2.3'
            assert '$(' not in str(info), f'Unexpanded build variable in {name}'
        share = plistlib.loads((generated/'NanocodexShare.plist').read_bytes())
        assert share['NSExtension']['NSExtensionPrincipalClass'] == 'NanocodexShare.ShareViewController'
        app = plistlib.loads((generated/'NanocodexInbox.plist').read_bytes())
        assert app['NSMicrophoneUsageDescription'] and app['NSLocalNetworkUsageDescription']
        assert app['CFBundleIcons']['CFBundlePrimaryIcon']['CFBundleIconFiles']
        for name, source in [('NanocodexInbox', 'NanocodexInbox/NanocodexInbox.iOS.entitlements'),
                             ('NanocodexShare', 'NanocodexShare/NanocodexShare.entitlements')]:
            assert (generated/f'{name}.entitlements').read_bytes() == (apple/source).read_bytes()
        assert (generated/'Bundles/NanocodexShare/CaptureWebPage.js').read_bytes() == (apple/'NanocodexShare/CaptureWebPage.js').read_bytes()
        assert (generated/'Bundles/NanocodexInbox/MOBILE_DEPENDENCY_NOTICES.md').read_bytes() == (apple/'MOBILE_DEPENDENCY_NOTICES.md').read_bytes()
        sizes = {Image.open(p).size for p in (generated/'Bundles/NanocodexInbox').glob('AppIcon*.png')}
        assert sizes == {(120, 120), (180, 180), (76, 76), (152, 152), (167, 167)}
        assert (generated/'Bundles/NanocodexInbox/GoogleG.png').read_bytes() == (apple/'NanocodexInbox/Assets.xcassets/GoogleG.imageset/google-g.png').read_bytes()
        package = json.loads((generated/'package.json').read_text())
        assert {t['name'] for t in package['targets']} == {'NanocodexInbox', 'NanocodexShare', 'NanocodexWidgets'}
        assert any(d['name'] == 'NanocodexVoice' for t in package['targets'] for d in t['dependencies'])
        trace.append('PASS: three bundles, versions, privacy text, share class, original resources, icons and entitlements.\n')
        before = tree_digest(generated)
        run(apple, '--build-number', '../bad', ok=False)
        assert tree_digest(generated) == before
        run(apple, '--version', 'garbage', ok=False)
        assert tree_digest(generated) == before
        asset = apple/'NanocodexInbox/Assets.xcassets/NewAsset.imageset'
        asset.mkdir()
        (asset/'Contents.json').write_text('{}')
        result = run(apple, ok=False)
        assert 'Asset catalog changed' in result.stderr
        assert tree_digest(generated) == before
        shutil.rmtree(asset)
        run(apple, '--version', '1.2.4', '--build-number', '43', '--sdk-version', '27.0')
        package = json.loads((generated/'package.json').read_text())
        main = next(t for t in package['targets'] if t['name'] == 'NanocodexInbox')
        assert 'CENTAUR_APP_INTENTS_27' in main['defines']
        assert plistlib.loads((generated/'NanocodexShare.plist').read_bytes())['CFBundleVersion'] == '43'
        trace.append('PASS: invalid input preserves previous staging; unsupported assets fail; valid next build recovers.\n')
    print('PASS: Linux packaging preparation journeys (no Apple compilation or signing).')
finally:
    (EVIDENCE/'packaging-journeys.log').write_text('\n'.join(trace))
