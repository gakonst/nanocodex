import os
from pathlib import Path
import subprocess
import shutil
import sys
import tempfile
import unittest

class InstallAutoConnectTests(unittest.TestCase):
    def test_unit_is_private_uses_session_environment_and_does_not_retry_ambiguous_input(self):
        text=(Path(__file__).parent/'install-autoconnect.sh').read_text()
        code=text.split('python3 - "$app" <<\'PY\'\n',1)[1].split('\nPY\n',1)[0]
        with tempfile.TemporaryDirectory() as tmp:
            home=Path(tmp);app=home/'app with spaces';app.mkdir()
            env={'HOME':tmp,'PATH':os.environ['PATH'],'HYPRLAND_INSTANCE_SIGNATURE':'must-not-persist'}
            subprocess.run(['python3','-',str(app)],input=code,text=True,env=env,check=True)
            unit=(home/'.config/systemd/user/nanocodex-wow-bridge.service').read_text()
            self.assertIn('Restart=no',unit)
            self.assertIn('UMask=0077',unit)
            self.assertIn(' -m transport.autoconnect ',unit)
            self.assertIn('--allow-input --duration 0 --key-hold-ms 1',unit)
            self.assertNotIn('must-not-persist',unit)
            self.assertEqual((app/'auto-bridge').stat().st_mode&0o777,0o700)
            self.assertIn('WorkingDirectory='+str(app),unit)
            # Validate with the real parser where systemd is available. Quoting
            # this directive looks plausible but makes the unit unloadable.
            if shutil.which('systemd-analyze'):
                binary=app/'.venv/bin/python'
                binary.parent.mkdir(parents=True)
                binary.symlink_to(sys.executable)
                unit_path=home/'.config/systemd/user/nanocodex-wow-bridge.service'
                result=subprocess.run(['systemd-analyze','verify','--man=no',str(unit_path)],capture_output=True,text=True)
                self.assertEqual(result.returncode,0,result.stdout+result.stderr)
