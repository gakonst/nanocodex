"""Start/stop a PRIVATE compositor. Requires existing staged Omarchy lab assets.

Never attaches to or modifies the user's compositor; every hyprctl call carries
an instance signature discovered under our newly-created private runtime dir.
"""
import argparse
import atexit
import json
import os
import signal
import subprocess
import time
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument('operation', choices=['start', 'stop', 'snapshot', 'keys'])
parser.add_argument('root', type=Path)
parser.add_argument('--assets', type=Path)
args = parser.parse_args()
root = args.root.resolve()

def matches(record):
    try:
        return Path(f"/proc/{record['pid']}/stat").read_text().rsplit(') ', 1)[1].split()[19] == record['start']
    except FileNotFoundError:
        return False


def cleanup(owned):
    if any(not isinstance(record, dict) for record in owned):
        raise RuntimeError('legacy ownership record: inspect identities before manual cleanup')
    for sig in (signal.SIGTERM, signal.SIGKILL):
        for record in reversed(owned):
            if matches(record):
                try:
                    os.killpg(record['pid'], sig)
                except ProcessLookupError:
                    pass
        if sig == signal.SIGTERM:
            time.sleep(1)


if args.operation == 'start':
    root.mkdir(parents=True, exist_ok=False)
    assets = args.assets.resolve()
    # Hyprland embeds its long instance signature in Unix socket paths.
    import secrets
    runtime = Path('/dev/shm/i'+secrets.token_hex(2))
    runtime.mkdir(mode=0o700)
    pids = []
    cleanup_failed_start = lambda: cleanup(pids)
    atexit.register(cleanup_failed_start)
    def launch(command, env, name, stdin=None):
        process = subprocess.Popen(command, env=env, stdout=open(root / (name+'.log'), 'w'),
            stderr=subprocess.STDOUT, start_new_session=True, stdin=stdin)
        pids.append(dict(pid=process.pid, start=Path(f'/proc/{process.pid}/stat').read_text().rsplit(') ', 1)[1].split()[19]))
        (root/'owned.json').write_text(json.dumps(dict(pids=pids, runtime=str(runtime))))
        return process
    labwc = assets/'labwc/usr'
    env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime), LD_LIBRARY_PATH=str(labwc/'lib'),
        WLR_BACKENDS='headless', WLR_HEADLESS_OUTPUTS='1', WLR_RENDERER='gles2',
        WLR_LIBINPUT_NO_DEVICES='1')
    launch([str(labwc/'bin/labwc'), '-C', str(assets/'labwc-config')], env, 'labwc')
    time.sleep(2)
    display = next(x.name for x in runtime.glob('wayland-*') if not x.name.endswith('lock'))
    config = root/'hyprland.conf'
    config.write_text('''monitor = HEADLESS-1,1920x1080@60,0x0,1
misc {
 disable_hyprland_logo = true
 disable_splash_rendering = true
}
animations {
 enabled = false
}
input {
 kb_layout = us
}
plugin:cua:enabled = true
exec-once = hyprctl output create headless HEADLESS-1
''')
    env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime), WAYLAND_DISPLAY=display, AQ_BACKEND='wayland')
    launch(['Hyprland', '--config', str(config)], env, 'hyprland')
    time.sleep(2)
    signature = next((runtime/'hypr').iterdir()).name
    env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime), WAYLAND_DISPLAY='wayland-1',
        HYPRLAND_INSTANCE_SIGNATURE=signature, GDK_BACKEND='wayland',
        NANOCODEX_COMPUTER_BACKGROUND='hyprland',
        NANOCODEX_HYPRLAND_CAPTURE=str(assets/'concurrency-staged/candidate/nanocodex-hyprland-capture'))
    def ctl(*command):
        return subprocess.check_output(['hyprctl', '-i', signature, *command], env=env, text=True, timeout=5)
    ctl('plugin', 'load', str(assets/'concurrency-staged/candidate/cua-hyprland-plugin.so'))
    # A headless seat needs a retained primary keymap for layout validation.
    keyboard_fifo = root/'keyboard.fifo'
    os.mkfifo(keyboard_fifo, 0o600)
    # Retain the same primary device: adding a keyboard mid-run correctly
    # revokes native connections as a desktop transition. RDWR avoids EOF.
    with os.fdopen(os.open(keyboard_fifo, os.O_RDWR), 'rb', buffering=0) as source:
        launch([str(assets/'bin/primary-keyboard')], env, 'primary-keymap', stdin=source)
    lanes = {}
    for index in range(8):
        process = launch(['python3', str(Path(__file__).with_name('fixture.py')), str(root), str(index)], env, f'fixture-{index}')
        lanes[str(index)] = dict(pid=process.pid, state=str(root/f'lane-{index}.json'), log=str(root/f'companion-{index}.log'))
    foreground = launch(['python3', str(Path(__file__).with_name('fixture.py')), str(root), '-1'], env, 'foreground')
    time.sleep(3)
    expected = {v['pid'] for v in lanes.values()} | {foreground.pid}
    deadline = time.monotonic()+15
    while True:
        clients = json.loads(ctl('clients', '-j'))
        if expected <= {w['pid'] for w in clients}:
            break
        if time.monotonic() >= deadline:
            raise TimeoutError('owned windows did not map within 15 seconds')
        time.sleep(.1)
    for index, pid in enumerate([v['pid'] for v in lanes.values()]+[foreground.pid]):
        window = next(w for w in clients if w['pid'] == pid)
        address = 'address:'+window['address']
        if not window['floating']:
            ctl('dispatch', 'setfloating', address)
        ctl('dispatch', 'resizewindowpixel', 'exact 340 220,'+address)
        ctl('dispatch', 'movewindowpixel', f'exact {20+(index%4)*450} {20+(index//4)*320},'+address)
    ctl('dispatch', 'focuswindow', 'address:'+next(w['address'] for w in clients if w['pid']==foreground.pid))
    safe_env = {k:env[k] for k in ('XDG_RUNTIME_DIR', 'WAYLAND_DISPLAY', 'HYPRLAND_INSTANCE_SIGNATURE', 'GDK_BACKEND', 'NANOCODEX_COMPUTER_BACKGROUND', 'NANOCODEX_HYPRLAND_CAPTURE')}
    (root/'config.json').write_text(json.dumps(dict(env=safe_env, lanes=lanes,
        companion=str(assets/'concurrency-staged/candidate/nanocodex-computer'))))
    (root/'assets.json').write_text(json.dumps(dict(keyboard=str(assets/'bin/primary-keyboard'))))
    atexit.unregister(cleanup_failed_start)
    print(root/'config.json')
elif args.operation == 'stop':
    # Only explicit owned processes, no killall and no live desktop calls.
    cleanup(json.loads((root/'owned.json').read_text())['pids'])
else:
    config = json.loads((root/'config.json').read_text())
    env = dict(os.environ, **config['env'])
    if args.operation == 'keys':
        fd = os.open(root/'keyboard.fifo', os.O_WRONLY | os.O_NONBLOCK)
        try:
            os.write(fd, b'17\n'*20)
        finally:
            os.close(fd)
    else:
        def ctl(*command):
            return json.loads(subprocess.check_output(['hyprctl', '-i', env['HYPRLAND_INSTANCE_SIGNATURE'], *command, '-j'], env=env))
        print(json.dumps(dict(focus=ctl('activewindow')['pid'], pointer=ctl('cursorpos'),
            foreground=json.loads((root/'lane--1.json').read_text()))))
