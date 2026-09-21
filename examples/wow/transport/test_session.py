import os
from pathlib import Path
import socket
import tempfile
import unittest
from unittest.mock import patch
from transport.session import desktop_session


class DesktopSessionTests(unittest.TestCase):
    def test_current_user_session_without_username_allowlist(self):
        with tempfile.TemporaryDirectory() as directory, socket.socket(socket.AF_UNIX) as display:
            display.bind(str(Path(directory) / 'wayland-test'))
            env = dict(XDG_RUNTIME_DIR=directory, WAYLAND_DISPLAY='wayland-test', HYPRLAND_INSTANCE_SIGNATURE='test')
            with patch('transport.session.pwd.getpwuid') as account:
                account.return_value.pw_name = 'any-desktop-user'
                self.assertEqual(desktop_session(env)['user'], 'any-desktop-user')
            with self.assertRaises(ValueError):
                desktop_session(env, uid=0)
            os.chmod(directory, 0o755)
            with self.assertRaises(ValueError):
                desktop_session(env)

    def test_missing_session_and_path_escape_rejected(self):
        with self.assertRaises(ValueError):
            desktop_session({})
        with tempfile.TemporaryDirectory() as directory:
            env = dict(XDG_RUNTIME_DIR=directory, WAYLAND_DISPLAY='../foreign', HYPRLAND_INSTANCE_SIGNATURE='test')
            with self.assertRaises(ValueError):
                desktop_session(env)

    def test_regular_file_is_not_a_desktop_socket(self):
        with tempfile.TemporaryDirectory() as directory:
            (Path(directory) / 'fake').touch()
            with self.assertRaises(ValueError):
                desktop_session(dict(XDG_RUNTIME_DIR=directory, WAYLAND_DISPLAY='fake', HYPRLAND_INSTANCE_SIGNATURE='test'))
