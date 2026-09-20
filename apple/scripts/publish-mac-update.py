#!/usr/bin/env python3
"""Validate a locally signed IPA and prepare or deploy the persistent OTA feed."""
import argparse
import datetime as dt
import hashlib
import html
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import tempfile
import zipfile

ORIGIN = 'https://nanocodex-ios-updates.gakonst.workers.dev'
BUNDLE = 'xyz.paradigm.centaur'
REPO = Path(__file__).resolve().parents[2]


def run(*args):
    return subprocess.check_output(args, stderr=subprocess.PIPE)


def build_number(value):
    value = str(value)
    if not re.fullmatch(r'[1-9][0-9]*', value):
        raise ValueError('Build must be a positive integer without leading zeroes')
    return int(value)


def manifest(version, build):
    build_number(build)
    return plistlib.dumps({'items': [{'assets': [{'kind': 'software-package',
        'url': f'{ORIGIN}/builds/{build}/Nanocodex.ipa'}], 'metadata': {
        'bundle-identifier': BUNDLE, 'bundle-version': str(build),
        'kind': 'software', 'title': f'Nanocodex {version}'}}]})


def policy(latest, build, existing_hash, ipa_hash):
    if existing_hash is not None and existing_hash != ipa_hash:
        raise ValueError('Immutable build already exists with different IPA bytes')
    if latest and build_number(latest['build']) > build_number(build):
        raise ValueError('Refusing to downgrade latest')


def page(version, build):
    url = f'itms-services://?action=download-manifest&url={ORIGIN}/builds/{build}/manifest.plist'
    return ('<!doctype html><html lang="en"><meta charset="utf-8">'
        '<meta name="viewport" content="width=device-width,initial-scale=1">'
        '<meta name="robots" content="noindex,nofollow"><title>Install Nanocodex</title>'
        '<style>body{font:17px system-ui;max-width:440px;margin:12vh auto;padding:24px;background:#101114;color:#f5f5f7;line-height:1.5}p{color:#b7b9c3}a{display:block;padding:16px;background:#c5ff72;color:#142000;border-radius:14px;text-align:center;text-decoration:none;font-weight:600}</style>'
        '<body><h1>Install Nanocodex</h1><p>Version ' + html.escape(str(version)) +
        ' · build ' + html.escape(str(build)) + '</p><p>Open this page in Safari on your '
        'registered iPhone, then tap Install and confirm the iOS installation prompt.</p>'
        '<p><a href="' + html.escape(url, quote=True) + '">Install Nanocodex</a></p>'
        '<p>After installation, open Settings → Nanocodex updates in the app for future updates.</p></body></html>')


def validate(ipa, device):
    with tempfile.TemporaryDirectory(prefix='nanocodex-ipa-') as directory:
        root = Path(directory).resolve()
        with zipfile.ZipFile(ipa) as archive:
            for item in archive.infolist():
                target = (root / item.filename).resolve()
                if not target.is_relative_to(root) or (item.external_attr >> 16) & 0o170000 == 0o120000:
                    raise ValueError('Unsafe path or symlink in IPA')
            archive.extractall(root)
        apps = list((root / 'Payload').glob('*.app'))
        if len(apps) != 1:
            raise ValueError('IPA must contain exactly one top-level app')
        app = apps[0]
        info = plistlib.loads((app / 'Info.plist').read_bytes())
        if info.get('CFBundleIdentifier') != BUNDLE:
            raise ValueError(f'Expected bundle {BUNDLE}')
        build = str(info['CFBundleVersion'])
        build_number(build)
        version = str(info['CFBundleShortVersionString'])
        run('codesign', '--verify', '--deep', '--strict', '--verbose=2', str(app))
        bundles = [app] + [p for p in app.rglob('*') if p.is_dir() and p.suffix in ('.app', '.appex')]
        for bundle in bundles:
            run('codesign', '--verify', '--strict', '--verbose=2', str(bundle))
            profile = plistlib.loads(run('security', 'cms', '-D', '-i', str(bundle / 'embedded.mobileprovision')))
            expiry = profile['ExpirationDate'].replace(tzinfo=dt.timezone.utc)
            if expiry <= dt.datetime.now(dt.timezone.utc):
                raise ValueError(f'Expired provisioning profile: {bundle.name}')
            devices = profile.get('ProvisionedDevices', [])
            if not devices and not profile.get('ProvisionsAllDevices', False):
                raise ValueError(f'Profile does not permit direct installation: {bundle.name}')
            if device and device not in devices and not profile.get('ProvisionsAllDevices', False):
                raise ValueError(f'Target device absent from profile: {bundle.name}')
            bundle_info = plistlib.loads((bundle / 'Info.plist').read_bytes())
            if str(bundle_info.get('CFBundleVersion', '')) != build:
                raise ValueError(f'Extension build mismatch: {bundle.name}')
            bundle_id = bundle_info['CFBundleIdentifier']
            allowed = profile.get('Entitlements', {}).get('application-identifier', '')
            prefixes = profile.get('ApplicationIdentifierPrefix', [])
            valid = any(allowed == f'{prefix}.{bundle_id}' or
                (allowed.endswith('.*') and f'{prefix}.{bundle_id}'.startswith(allowed[:-1])) for prefix in prefixes)
            if not valid:
                raise ValueError(f'Profile application identifier mismatch: {bundle.name}')
        return version, build


HEADERS = '''/*
  X-Robots-Tag: noindex, nofollow
/
  Cache-Control: no-store
/index.html
  Cache-Control: no-store
/latest.json
  Cache-Control: no-store
/builds/*
  Cache-Control: public, max-age=31536000, immutable
/builds/:build/Nanocodex.ipa
  Content-Type: application/octet-stream
/builds/:build/manifest.plist
  Content-Type: text/xml
'''


def atomic_write(path, data):
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as tmp:
        tmp.write(data)
        temporary = Path(tmp.name)
    temporary.replace(path)


def prepare(ipa, assets, version, build, notes):
    digest = hashlib.sha256(ipa.read_bytes()).hexdigest()
    latest_path = assets / 'latest.json'
    latest = json.loads(latest_path.read_text()) if latest_path.exists() else None
    destination = assets / 'builds' / build
    old_ipa = destination / 'Nanocodex.ipa'
    old_hash = hashlib.sha256(old_ipa.read_bytes()).hexdigest() if old_ipa.exists() else None
    policy(latest, build, old_hash, digest)
    if destination.exists():
        if old_hash is None or (destination / 'manifest.plist').read_bytes() != manifest(version, build):
            raise ValueError('Existing immutable build is incomplete or metadata differs')
    else:
        destination.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=destination.parent, prefix='.prepare-') as staging:
            stage = Path(staging)
            shutil.copyfile(ipa, stage / 'Nanocodex.ipa')
            (stage / 'manifest.plist').write_bytes(manifest(version, build))
            (stage / 'index.html').write_text(page(version, build))
            (stage / 'sha256.txt').write_text(f'{digest}  Nanocodex.ipa\n')
            stage.rename(destination)
    receipt = {'version': version, 'build': build, 'bundle_id': BUNDLE,
        'manifest_url': f'{ORIGIN}/builds/{build}/manifest.plist',
        'published_at': dt.datetime.now(dt.timezone.utc).isoformat().replace('+00:00', 'Z')}
    if notes:
        receipt['notes'] = notes
    atomic_write(assets / '_headers', HEADERS.encode())
    atomic_write(assets / 'index.html', page(version, build).encode())
    atomic_write(latest_path, (json.dumps(receipt, indent=2) + '\n').encode())
    print(json.dumps(receipt, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--ipa', type=Path, required=True)
    parser.add_argument('--assets-dir', type=Path, required=True, help='Persistent directory outside the repository')
    parser.add_argument('--device-udid', default=os.environ.get('NANOCODEX_DEVICE_UDID'))
    parser.add_argument('--notes')
    parser.add_argument('--deploy', action='store_true')
    parser.add_argument('--config', type=Path, default=REPO / 'apple/ota/wrangler.jsonc')
    args = parser.parse_args()
    assets = args.assets_dir.expanduser().resolve()
    if assets.is_relative_to(REPO) or REPO.is_relative_to(assets):
        parser.error('--assets-dir must be a dedicated persistent directory outside the repository')
    assets.mkdir(parents=True, exist_ok=True)
    # One host-local lock serializes preparation and deployment of the entire feed.
    import fcntl
    with (assets.parent / ('.' + assets.name + '.ota.lock')).open('w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        version, build = validate(args.ipa, args.device_udid)
        prepare(args.ipa, assets, version, build, args.notes)
        if args.deploy:
            subprocess.run(['pnpm', '--filter', 'nanocodex-managed-service', 'exec', 'wrangler', 'deploy', '--config', str(args.config.resolve()),
                '--assets', str(assets)], cwd=REPO, check=True)


if __name__ == '__main__':
    try:
        main()
    except (ValueError, OSError, KeyError, subprocess.CalledProcessError) as error:
        raise SystemExit(f'OTA publication failed: {error}')
