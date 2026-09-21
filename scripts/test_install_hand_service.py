import importlib.util
from pathlib import Path
import plistlib
import unittest

spec = importlib.util.spec_from_file_location('hand_service', Path(__file__).with_name('install-hand-service.py'))
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)


class BootService(unittest.TestCase):
    def test_macos_runs_at_boot_as_owner_without_client_or_credentials(self):
        path, data = installer.service('darwin', 'fixture', '/Users/Fixture User')
        job = plistlib.loads(data)
        self.assertEqual(path.parent, Path('/Library/LaunchDaemons'))
        self.assertTrue(job['RunAtLoad'])
        self.assertEqual(job['UserName'], 'fixture')
        self.assertEqual(job['EnvironmentVariables']['HOME'], '/Users/Fixture User')
        self.assertEqual(job['ProgramArguments'][1:], ['hand'])
        self.assertNotIn('NANOCODEX_API_KEY', job['EnvironmentVariables'])

    def test_custom_login_selection_on_both_platforms(self):
        origin, account = 'https://fixture.example', '/home/Fixture User/account%2.json'
        for system in ['darwin', 'linux']:
            _, data = installer.service(system, 'fixture', '/home/fixture', origin, account)
            if system == 'darwin':
                environment = plistlib.loads(data)['EnvironmentVariables']
                self.assertEqual(environment['NANOCODEX_MANAGED_URL'], origin)
                self.assertEqual(environment['NANOCODEX_ACCOUNT_FILE'], account)
            else:
                self.assertIn('"NANOCODEX_MANAGED_URL=' + origin + '"', data.decode())
                self.assertIn('"NANOCODEX_ACCOUNT_FILE=' + account.replace('%', '%%') + '"', data.decode())

    def test_linux_machine_boot_target_and_literal_home(self):
        path, data = installer.service('linux', 'fixture', '/home/fixture%name')
        unit = data.decode()
        self.assertEqual(path.name, 'nanocodex-hand.service')
        self.assertIn('WantedBy=multi-user.target', unit)
        self.assertIn('User=fixture\n', unit)
        self.assertIn('HOME=/home/fixture%%name', unit)
        self.assertNotIn('parent-pipe', unit)
        self.assertNotIn('NANOCODEX_API_KEY', unit)


if __name__ == '__main__':
    unittest.main()
