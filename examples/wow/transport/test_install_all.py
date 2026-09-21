"""Installer lifecycle tests using temporary homes and fake commands, never services."""
import os
from pathlib import Path
import pty
import select
import shutil
import subprocess
import sys
import tempfile
import time
import unittest


ROOT = Path(__file__).resolve().parent.parent


class InstallAllTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='ncw installer ')
        self.addCleanup(self.temp.cleanup)
        self.tree = Path(self.temp.name)
        self.source = self.tree / 'source with spaces'
        self.home = self.tree / 'home'
        self.commands = self.tree / 'commands'
        self.runtime = self.tree / 'runtime'
        self.client = self.tree / 'WoW client'
        self.log = self.tree / 'calls.log'
        for directory in (self.source / 'scripts', self.source / 'transport',
                          self.home / '.local/bin', self.commands, self.runtime,
                          self.client / 'Interface'):
            directory.mkdir(parents=True)
        self.script = self.source / 'scripts/install-all.sh'
        shutil.copyfile(ROOT / 'scripts/install-all.sh', self.script)
        self.executable(self.commands / 'id', "printf '1000\\n'\n")
        self.executable(self.commands / 'systemctl', '''printf 'systemctl|%s\\n' "$*" >> "$TEST_LOG"
if [[ ${2:-} == is-active ]]; then
  [[ ${TEST_ACTIVE:-0} == 1 ]]
else
  exit 0
fi
''')
        for name, path in [('desktop', 'scripts/install-desktop.sh'),
                           ('transport', 'transport/install.sh'),
                           ('addon', 'scripts/install-addon.sh'),
                           ('autoconnect', 'transport/install-autoconnect.sh')]:
            self.executable(self.source / path,
                            f'''printf '{name}|%s\\n' "$*" >> "$TEST_LOG"\n''')
        self.executable(self.home / '.local/bin/nanocodex-wow',
                        '''printf 'companion|%s\\n' "$*" >> "$TEST_LOG"\n''')
        # Deliberately do not inherit desktop, account, or credential variables.
        self.env = {'HOME': str(self.home), 'PATH': f'{self.commands}:/usr/bin:/bin',
                    'XDG_RUNTIME_DIR': str(self.runtime), 'TEST_LOG': str(self.log),
                    'TEST_ACTIVE': '0'}

    @staticmethod
    def executable(path, body):
        path.write_text('#!/bin/bash\nset -euo pipefail\n' + body)
        path.chmod(0o700)

    def calls(self):
        return self.log.read_text().splitlines() if self.log.exists() else []

    def command(self):
        return ['/bin/bash', str(self.script), str(self.client)]

    def test_active_bridge_refuses_before_any_installer(self):
        self.env['TEST_ACTIVE'] = '1'
        result = subprocess.run(self.command(), env=self.env, input='',
                                text=True, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 1)
        self.assertIn('already running', result.stderr)
        self.assertEqual(self.calls(), ['systemctl|--user is-active --quiet nanocodex-wow-bridge.service'])

    def test_non_tty_installs_deferred_without_starting_or_opening(self):
        result = subprocess.run(self.command(), env=self.env, input='',
                                text=True, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 2)
        self.assertEqual(self.calls(), [
            'systemctl|--user is-active --quiet nanocodex-wow-bridge.service',
            'desktop|', 'transport|', f'addon|{self.client}', 'autoconnect|--defer-start'])
        self.assertIn('After /reload: systemctl --user start nanocodex-wow-bridge.service', result.stdout)
        self.assertIn('Connection has not started', result.stdout)

    def start_pty_until_prompt(self):
        master, slave = pty.openpty()
        self.addCleanup(os.close, master)
        process = subprocess.Popen(self.command(), env=self.env, stdin=slave,
                                   stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        os.close(slave)
        def cleanup():
            if process.poll() is None:
                process.kill()
            process.wait(timeout=5)
            process.stdout.close()
        self.addCleanup(cleanup)
        output = b''
        deadline = time.monotonic() + 5
        while b'press Enter.' not in output:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                self.fail('Installer did not reach reload prompt: ' + repr(output))
            if select.select([process.stdout], [], [], remaining)[0]:
                chunk = os.read(process.stdout.fileno(), 4096)
                if not chunk:
                    self.fail('Installer exited before reload prompt: ' + repr(output))
                output += chunk
        # A missing read must not escape detection through a process scheduling race.
        with self.assertRaises(subprocess.TimeoutExpired):
            process.wait(timeout=.1)
        self.assertEqual(self.calls()[-1], 'autoconnect|--defer-start')
        self.assertFalse(any('--user start ' in call or call.startswith('companion|') for call in self.calls()))
        return process, master, output

    def test_interactive_starts_and_opens_only_after_enter(self):
        process, master, prefix = self.start_pty_until_prompt()
        os.write(master, b'\n')
        output, _ = process.communicate(timeout=5)
        self.assertEqual(process.returncode, 0, (prefix + output).decode())
        self.assertEqual(self.calls()[-2:], [
            'systemctl|--user start nanocodex-wow-bridge.service', 'companion|home'])
        self.assertEqual(sum('--user start ' in call for call in self.calls()), 1)
        self.assertIn(b'Companion opened', output)

    def test_interactive_eof_does_not_start(self):
        process, master, _ = self.start_pty_until_prompt()
        os.write(master, b'\x04')  # Canonical terminal EOF with an empty input line.
        process.communicate(timeout=5)
        self.assertNotEqual(process.returncode, 0)
        self.assertEqual(self.calls()[-1], 'autoconnect|--defer-start')
        self.assertFalse(any('--user start ' in call or call.startswith('companion|') for call in self.calls()))

    def test_actual_autoconnect_defer_flag_never_starts_service(self):
        script = self.source / 'transport/install-autoconnect.sh'
        shutil.copyfile(ROOT / 'transport/install-autoconnect.sh', script)
        app = self.home / '.local/share/nanocodex-wow'
        (app / '.venv/bin').mkdir(parents=True)
        # Stub only the session check; run the real unit generation in a temp home.
        self.executable(app / '.venv/bin/python', 'cat >/dev/null\n')
        (self.commands / 'python3').symlink_to(sys.executable)
        result = subprocess.run(['/bin/bash', str(script), '--defer-start'],
                                env=self.env, input='', text=True, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls(), [
            'systemctl|--user import-environment XDG_RUNTIME_DIR WAYLAND_DISPLAY HYPRLAND_INSTANCE_SIGNATURE',
            'systemctl|--user daemon-reload',
            'systemctl|--user enable nanocodex-wow-bridge.service'])
        self.assertIn('start deferred', result.stdout)
        unit = (self.home / '.config/systemd/user/nanocodex-wow-bridge.service').read_text()
        self.assertIn('Restart=no\n', unit)
        self.assertIn('--allow-input --duration 0 --key-hold-ms 1', unit)
        self.assertEqual((app / 'auto-bridge').stat().st_mode & 0o777, 0o700)


if __name__ == '__main__':
    unittest.main()
