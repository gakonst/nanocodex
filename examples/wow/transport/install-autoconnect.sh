#!/usr/bin/env bash
set -euo pipefail
start_now=true
if [[ ${1:-} == --defer-start && $# == 1 ]]; then
  start_now=false
elif [[ $# != 0 ]]; then
  echo 'Usage: install-autoconnect.sh [--defer-start]' >&2; exit 2
fi
app="$HOME/.local/share/nanocodex-wow"
if [[ $(id -u) == 0 || -z ${XDG_RUNTIME_DIR:-} || ! -O "$XDG_RUNTIME_DIR" ]]; then
  echo 'Run as the logged-in desktop user.' >&2; exit 1
fi
cd "$app"
"$app/.venv/bin/python" - <<'PY'
from transport.session import desktop_session
desktop_session()
PY
python3 - "$app" <<'PY'
import os,pathlib,sys
app=pathlib.Path(sys.argv[1]);state=app/'auto-bridge'
state.mkdir(mode=0o700,exist_ok=True);state.chmod(0o700)
def quote(value):return '"'+str(value).replace('\\','\\\\').replace('"','\\"').replace('%','%%')+'"'
lines=['[Unit]','Description=Nanocodex automatic WoW connection','After=graphical-session-pre.target network.target nanocodex-wow.service','PartOf=graphical-session.target','[Service]','Type=simple','WorkingDirectory='+str(app).replace('%','%%'),'UMask=0077',
       'ExecStart='+quote(app/'.venv/bin/python')+' -m transport.autoconnect --state-dir '+quote(state)+' --evidence '+quote(state/'status.json')+' --allow-input --duration 0 --key-hold-ms 1',
       'Restart=no','TimeoutStopSec=45']
for name in ('CODEX_HOME','NANOCODEX_ACCOUNT_FILE','NANOCODEX_MANAGED_URL'):
 if os.environ.get(name):lines.append('Environment='+quote(name+'='+os.environ[name]))
lines+=['[Install]','WantedBy=graphical-session.target','']
unit=pathlib.Path.home()/'.config/systemd/user/nanocodex-wow-bridge.service'
unit.parent.mkdir(parents=True,exist_ok=True);unit.write_text('\n'.join(lines))
PY
systemctl --user import-environment XDG_RUNTIME_DIR WAYLAND_DISPLAY HYPRLAND_INSTANCE_SIGNATURE
systemctl --user daemon-reload
systemctl --user enable nanocodex-wow-bridge.service
if $start_now; then
  systemctl --user start nanocodex-wow-bridge.service
  printf 'Automatic WoW connection service installed and started.\n'
else
  printf 'Automatic WoW connection installed; start deferred until the updated addon is loaded.\n'
fi
