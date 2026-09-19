"""Exercise installer unit generation without a desktop or service mutations."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class InstallerUnits(unittest.TestCase):
    def test_fresh_user_and_existing_unit(self):
        script = (Path(__file__).parent / 'install.sh').read_text()
        code = script.split('python3 - "$app" <<\'PY\'\n', 1)[1].split('\nPY\n', 1)[0]
        with tempfile.TemporaryDirectory() as root:
            home = Path(root)
            units = home / '.config/systemd/user'
            (units / 'nanocodex-wow.service.d').mkdir(parents=True)
            app = home / 'app with spaces'
            env = {'HOME': root, 'PATH': os.environ['PATH']}
            subprocess.run(['python3', '-', str(app)], input=code, text=True, env=env, check=True)
            unit = units / 'nanocodex-wow.service'
            self.assertIn('UMask=0077', unit.read_text())
            self.assertIn('WorkingDirectory="' + str(app) + '"', unit.read_text())
            override = units / 'nanocodex-wow.service.d/streaming.conf'
            self.assertIn('durable_client.py" --port 17840', override.read_text())
            unit.write_text('[Service]\n# Existing user configuration\n')
            subprocess.run(['python3', '-', str(app)], input=code, text=True, env=env, check=True)
            self.assertEqual(unit.read_text(), '[Service]\n# Existing user configuration\n')
