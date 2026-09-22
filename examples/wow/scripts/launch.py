#!/usr/bin/env python3
"""Focus the companion and route actions with the compositor's current API."""
import fcntl
import json
from pathlib import Path
import re
import subprocess
import sys
import time


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else 'home'
    if mode not in ('home', 'projects', 'voice'):
        raise SystemExit('Usage: nanocodex-wow [home|projects|voice]')
    profile = Path.home() / '.local/share/nanocodex-wow/browser-profile'
    profile.parent.mkdir(parents=True, exist_ok=True)
    with (profile.parent / 'launcher.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        subprocess.run(['systemctl', '--user', 'start', 'nanocodex-wow.service'], check=True, timeout=15)
        try:
            version = json.loads(subprocess.check_output(['hyprctl', 'version', '-j'], timeout=3))
            match = re.search(r'(\d+)\.(\d+)', str(version.get('version') or version.get('tag') or ''))
            modern = bool(match and tuple(map(int, match.groups())) >= (0, 55))
        except (OSError, ValueError, subprocess.SubprocessError):
            modern = False

        def dispatch(lua, *legacy):
            args = [lua] if modern else list(legacy)
            subprocess.run(['hyprctl', 'dispatch', *args], stdout=subprocess.DEVNULL,
                           stderr=subprocess.DEVNULL, timeout=3, check=True)

        def window():
            try:
                rows = json.loads(subprocess.check_output(['hyprctl', 'clients', '-j'], timeout=3, stderr=subprocess.DEVNULL))
                return next((r['address'] for r in rows if
                    r.get('class') == 'nanocodex-wow' or r.get('initialClass') == 'nanocodex-wow' or
                    (r.get('title') == 'Nanocodex · Azeroth companion' and
                     ('chrom' in r.get('class', '').lower() or 'chrom' in r.get('initialClass', '').lower()))), None)
            except (OSError, ValueError, subprocess.SubprocessError):
                return None

        address = window()
        existed = bool(address)
        if not address:
            subprocess.Popen(['chromium', '--user-data-dir=' + str(profile), '--class=nanocodex-wow',
                '--app=http://127.0.0.1:17840/#' + mode, '--window-size=1180,820'],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
            for _ in range(50):
                time.sleep(0.1)
                address = window()
                if address:
                    break
        if address:
            selector = 'address:' + address
            encoded = json.dumps(selector)
            dispatch(f'hl.dsp.window.float({{action="set",window={encoded}}})', 'setfloating', selector)
            dispatch(f'hl.dsp.window.resize({{x=1180,y=820,window={encoded}}})', 'resizewindowpixel', 'exact 1180 820,' + selector)
            dispatch(f'hl.dsp.focus({{window={encoded}}})', 'focuswindow', selector)
            dispatch(f'hl.dsp.window.alter_zorder({{mode="top",window={encoded}}})', 'alterzorder', 'top,' + selector)
            if existed:
                time.sleep(0.15)
                key = {'home': 'n', 'projects': 'p', 'voice': 'v'}[mode]
                dispatch(f'hl.dsp.send_shortcut({{mods="ALT SHIFT",key="{key}",window={encoded}}})',
                         'sendshortcut', 'ALT SHIFT,' + key + ',' + selector)


if __name__ == '__main__':
    main()
