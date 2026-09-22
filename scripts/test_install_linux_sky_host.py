import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class ManagedReceiptTest(unittest.TestCase):
    def test_registration_is_opt_in_and_publishes_complete_launcher_receipt(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            runtime = root / 'runtime'
            for path in [runtime / 'bin/node', runtime / 'bin/node_repl', root / 'codex',
                         runtime / 'lib/node_modules/@oai/cua-repl/bin/cua-repl.mjs',
                         runtime / 'lib/node_modules/@oai/sky/dist/project/cua/sky_js/src/service.js']:
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text('#!/bin/sh\nexit 0\n')
                path.chmod(0o755)
            env = dict(os.environ, NANOCODEX_DIR=str(root / 'managed'))
            command = ['python3', str(Path(__file__).with_name('install-linux-sky-host.py')),
                       '--runtime', str(runtime), '--codex-cli', str(root / 'codex')]
            receipt_path = root / 'managed/runtimes/openai-cua/provider.json'
            subprocess.run(command + ['--destination', str(root / 'unselected')],
                           env=env, check=True, capture_output=True)
            self.assertFalse(receipt_path.exists())
            # Replace an existing selection only after preparing its launcher.
            receipt_path.parent.mkdir(parents=True)
            receipt_path.write_text('{}')
            subprocess.run(command + ['--destination', str(root / 'selected'), '--register-managed'],
                           env=env, check=True, capture_output=True)
            receipt = json.loads(receipt_path.read_text())
            self.assertEqual(receipt, dict(status='installed', transport='mcp',
                             executable=str(root / 'selected/cua-provider'), args=[], environment={}))
            self.assertTrue(os.access(receipt['executable'], os.X_OK))
            subprocess.run(['/bin/sh', '-n', receipt['executable']], check=True)
            self.assertEqual(list(receipt_path.parent.glob('.provider-*')), [])


if __name__ == '__main__':
    unittest.main()
