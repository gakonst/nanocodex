#!/usr/bin/python3
"""Load a tested module once. Never unload, replace, or restart a compositor."""
import hashlib, json, os, pathlib, subprocess, sys
root = pathlib.Path('/opt/nanocodex/background-cua')
if os.geteuid() == 0: sys.exit('Run as the desktop user')
env = {'PATH': '/usr/bin:/bin', 'HOME': str(pathlib.Path.home()), 'XDG_RUNTIME_DIR': f'/run/user/{os.geteuid()}'}
instances = json.loads(subprocess.check_output(['hyprctl', 'instances', '-j'], env=env, timeout=3))
if len(instances) != 1: sys.exit('Expected exactly one desktop compositor')
signature = instances[0]['instance']
def ctl(*args):
    return subprocess.check_output(['hyprctl', '-i', signature, *args], env=env, timeout=5).decode()
version = json.loads(ctl('version', '-j'))
expected = json.loads((root / 'compositor-version.json').read_text())
identity = ('commit', 'version', 'abiHash', 'dirty')
if (any(key not in expected or key not in version or version[key] != expected[key] for key in identity)
        or expected['dirty'] is not False
        or any(not isinstance(expected[key], str) or not expected[key] for key in identity[:3])):
    sys.exit('Compositor differs from tested build; rebuild and validate first')
plugins = json.loads(ctl('plugin', 'list', '-j'))
marker = pathlib.Path(env['XDG_RUNTIME_DIR']) / 'nanocodex-background-cua.json'
receipt = {'instance': signature, 'pid': instances[0]['pid'], 'sha256': hashlib.sha256((root / 'cua-hyprland-plugin.so').read_bytes()).hexdigest()}
if plugins:
    if marker.is_file() and json.loads(marker.read_text()) == receipt:
        status = json.loads(ctl('cua:status', '-j'))
        if status.get('abi', {}).get('match') and status.get('input', {}).get('transport_ready'): sys.exit(0)
    sys.exit('Existing plugins require explicit review; refusing module replacement')
print(ctl('plugin', 'load', str(root / 'cua-hyprland-plugin.so')))
configured = ctl('keyword', 'plugin:cua:enabled', 'true')
if 'non-legacy parsers' in configured:
    configured = ctl('eval', 'hl.config({ plugin = { cua = { enabled = true } } })')
if configured.strip() != 'ok': sys.exit('Plugin configuration refused: ' + configured.strip())
print(configured)
status = json.loads(ctl('cua:refresh', '-j'))
if not status.get('abi', {}).get('match') or not status.get('input', {}).get('transport_ready'):
    sys.exit('Plugin did not establish background input transport')
marker.write_text(json.dumps(receipt)); marker.chmod(0o600)
print('Plugin activated and recorded for this compositor lifetime.')
