#!/usr/bin/env bash
# Run directly from the logged-in desktop user's terminal; never sudo/su.
set -euo pipefail
if [[ $(id -u) == 0 || -z ${XDG_RUNTIME_DIR:-} || ! -O "$XDG_RUNTIME_DIR" ]]; then
  echo 'Run as the current desktop user in their own session.' >&2; exit 1
fi
source_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
app="$HOME/.local/share/nanocodex-wow"
mkdir -p "$app" "$HOME/.config/systemd/user/nanocodex-wow.service.d"
chmod 700 "$app"
for module in server.py durable_client.py settings.py voice.py; do install -m 600 "$source_root/$module" "$app/$module"; done
mkdir -p "$app/transport"
chmod 700 "$app/transport"
for module in "$source_root"/transport/*.py; do install -m 600 "$module" "$app/transport/"; done
mkdir -p "$app/transport/native"
for file in carrier-keys.c virtual-keyboard.xml build.sh; do install -m 600 "$source_root/transport/native/$file" "$app/transport/native/$file"; done
bash "$app/transport/native/build.sh"
python3 -m venv "$app/.venv"
"$app/.venv/bin/python" -m pip install --disable-pip-version-check 'websockets>=15,<16' Pillow
python3 - "$app" <<'PY'
import os,pathlib,sys
app=pathlib.Path(sys.argv[1])
def quote(value): return '"'+str(value).replace('\\','\\\\').replace('"','\\"')+'"'
lines=['[Service]','ExecStart=', 'ExecStart='+quote(app/'.venv/bin/python')+' '+quote(app/'durable_client.py')+' --port 17840']
for name in ('CODEX_HOME','NANOCODEX_ACCOUNT_FILE','NANOCODEX_MANAGED_URL'):
    if os.environ.get(name): lines.append('Environment='+quote(name+'='+os.environ[name]))
unit_root=pathlib.Path.home()/'.config/systemd/user'
unit=unit_root/'nanocodex-wow.service'
# Fresh desktop users do not have the old web-companion service. Create only
# our missing unit; preserve an existing installation's base configuration.
if not unit.exists():
    unit.write_text('\n'.join([
        '[Unit]', 'Description=Nanocodex WoW streaming companion', 'After=network.target',
        '[Service]', 'Type=simple', 'WorkingDirectory='+quote(app),
        'ExecStart='+quote(app/'.venv/bin/python')+' '+quote(app/'durable_client.py')+' --port 17840',
        'Restart=on-failure', 'RestartSec=3', 'UMask=0077',
        '[Install]', 'WantedBy=default.target', '']))
(unit_root/'nanocodex-wow.service.d/streaming.conf').write_text('\n'.join(lines)+'\n')
PY
systemctl --user daemon-reload
systemctl --user enable nanocodex-wow.service
systemctl --user restart nanocodex-wow.service
printf 'Streaming companion installed for %s.\n' "$(id -un)"
