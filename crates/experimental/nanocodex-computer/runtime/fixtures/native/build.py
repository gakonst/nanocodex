#!/usr/bin/env python3
"""Build only. UI launch/run is a separate CUA-controlled operation."""
import argparse, datetime, hashlib, json, pathlib, shlex, shutil, subprocess
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--reuse-launcher',type=pathlib.Path,help='Copy an existing owned fixture app byte-for-byte, preserving its signing requirement. Never recompiles or resigns that launcher.')
parser.add_argument('--binary',type=pathlib.Path,help='Explicit Rust helper to pin; defaults to target/debug/nanocodex-computer.')
parser.add_argument('--script',type=pathlib.Path,help='Explicit owned conformance script to pin.')
parser.add_argument('--preapprove-owned-fixture',action='store_true',help='Write trusted local policy approving only this exact owned bundle path. This does not grant macOS Accessibility or Screen Recording permission.')
args=parser.parse_args()
root=pathlib.Path(__file__).resolve().parents[2]
folder=root/'artifacts'/('native-conformance-'+datetime.datetime.now(datetime.UTC).strftime('%Y%m%dT%H%M%S%fZ'))
bundle=folder/'Skyre Native Conformance.app'
folder.mkdir(parents=True,mode=0o700)
plist='''<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>CFBundleExecutable</key><string>Fixture</string><key>CFBundleIdentifier</key><string>org.skyre.rebuild.conformance.20260906</string><key>CFBundleName</key><string>Skyre Native Conformance</string><key>CFBundlePackageType</key><string>APPL</string><key>NSHighResolutionCapable</key><true/><key>NSScreenCaptureUsageDescription</key><string>Validate screenshot and optional loopback audio behavior using owned synthetic fixtures.</string></dict></plist>'''
if args.reuse_launcher:
    import plistlib
    launcher=args.reuse_launcher.resolve()
    info=plistlib.loads((launcher/'Contents/Info.plist').read_bytes())
    if info.get('CFBundleIdentifier')!='org.skyre.rebuild.conformance.20260906':
        raise SystemExit('Only an owned Skyre conformance launcher can be reused')
    subprocess.run(['codesign','--verify','--strict',str(launcher)],check=True)
    shutil.copytree(launcher,bundle,symlinks=True)
    subprocess.run(['codesign','--verify','--strict',str(bundle)],check=True)
    launcher_files={str(p.relative_to(launcher)):hashlib.sha256(p.read_bytes()).hexdigest() for p in launcher.rglob('*') if p.is_file()}
    copied_files={str(p.relative_to(bundle)):hashlib.sha256(p.read_bytes()).hexdigest() for p in bundle.rglob('*') if p.is_file()}
    if launcher_files!=copied_files: raise SystemExit('Copied launcher bytes differ')
    shutil.copy2(launcher.parent/'Fixture.swift',folder/'Fixture.swift')
else:
    (bundle/'Contents/MacOS').mkdir(parents=True)
    (bundle/'Contents/Info.plist').write_text(plist)
    subprocess.run(['xcrun','swiftc','-swift-version','5',str(pathlib.Path(__file__).with_name('Fixture.swift')),'-o',str(bundle/'Contents/MacOS/Fixture')],check=True)
    subprocess.run(['codesign','--force','--sign','-',str(bundle)],check=True)
    shutil.copy2(pathlib.Path(__file__).with_name('Fixture.swift'),folder/'Fixture.swift')
helper_name='skyre-real' if args.preapprove_owned_fixture else 'skyre'
shutil.copy2(args.binary or root/'target/debug/nanocodex-computer', folder/helper_name)
script=(args.script or pathlib.Path(__file__).with_name('conformance.js')).read_text()
if not args.script and script.count('"SKYRE_OWNED_FIXTURE_APP"')!=1:
    raise SystemExit('Expected exactly one owned fixture target token')
script=script.replace('"SKYRE_OWNED_FIXTURE_APP"',json.dumps(str(bundle)))
(folder/'conformance.js').write_text(script)
if args.preapprove_owned_fixture:
    policy=folder/'owned-fixture-policy.json'
    policy.write_text(json.dumps({'allowed_apps':[str(bundle)],'preapproved_apps':[str(bundle)]},indent=2)+'\n')
    policy.chmod(0o600)
    # Preserve the launcher's signed bytes and its child PID. The production
    # runtime's approval boundary remains enforced with explicit CLI policy.
    wrapper=folder/'skyre'
    wrapper.write_text('#!/bin/sh\nexec '+shlex.quote(str(folder/helper_name))+' --security-config '+shlex.quote(str(policy))+' "$@"\n')
    wrapper.chmod(0o700)
artifacts={str(p.relative_to(folder)):hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(folder.rglob('*')) if p.is_file()}
sources={str(p.relative_to(root)):hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted((root/'src').rglob('*')) if p.is_file()}
(folder/'build.json').write_text(json.dumps({'bundle':str(bundle),'binary':str(folder/helper_name),'launcher_entry':str(folder/'skyre'),'built_at':datetime.datetime.now(datetime.UTC).isoformat(),'reused_launcher':str(args.reuse_launcher.resolve()) if args.reuse_launcher else None,'helper_source':str((args.binary or root/'target/debug/nanocodex-computer').resolve()),'preapproved_owned_fixture_only':args.preapprove_owned_fixture,'artifacts':artifacts,'source_hashes_at_pin_time':sources},indent=2)+'\n')
print(bundle)
