#!/usr/bin/env python3
"""Import an existing key through the official CLI, with hidden terminal input."""
import getpass
import subprocess
key = getpass.getpass('Nanocodex account API key (hidden): ').strip()
if not key:
    raise SystemExit('No key entered; nothing changed.')
result = subprocess.run(['/opt/nanocodex/current/nanocodex2', 'login', '--with-api-key'],
                        input=key+'\n', text=True, capture_output=True)
key = None
if result.returncode:
    raise SystemExit('Account import failed. Check the key and account origin; no credential was printed.')
print('Account connected. Refresh the WoW companion.')
