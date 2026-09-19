"""Validate the caller's own desktop session without a username allowlist."""
import os
from pathlib import Path
import pwd
import stat


def desktop_session(environ=None, uid=None):
    environ = os.environ if environ is None else environ
    uid = os.geteuid() if uid is None else uid
    if uid == 0:
        raise ValueError('run directly as the logged-in desktop user, not root')
    runtime_value = environ.get('XDG_RUNTIME_DIR')
    display = environ.get('WAYLAND_DISPLAY')
    if not runtime_value or not display or not environ.get('HYPRLAND_INSTANCE_SIGNATURE'):
        raise ValueError('run from the active Omarchy desktop session')
    runtime = Path(runtime_value)
    info = runtime.lstat()
    if not runtime.is_absolute() or not stat.S_ISDIR(info.st_mode) or info.st_uid != uid or info.st_mode & 0o077:
        raise ValueError('desktop runtime must be private and owned by the current user')
    endpoint = Path(display) if Path(display).is_absolute() else runtime / display
    if endpoint.parent != runtime or endpoint.name in ('', '.', '..'):
        raise ValueError('Wayland display must be inside the current user runtime')
    info = endpoint.lstat()
    if not stat.S_ISSOCK(info.st_mode) or info.st_uid != uid:
        raise ValueError('Wayland display must be a socket owned by the current user')
    return {'uid': uid, 'user': pwd.getpwuid(uid).pw_name, 'runtime': str(runtime), 'display': str(endpoint)}
