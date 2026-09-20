#!/usr/bin/env bash
set -euo pipefail
src=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
app="$HOME/.local/share/nanocodex-wow"
bin="$HOME/.local/bin"
mkdir -p "$app" "$bin" "$HOME/.config/systemd/user" "$HOME/.local/share/applications"
chmod 700 "$app"
if [[ "$src" != "$app" ]]; then
  cp -R "$src/web" "$src/addon" "$src/scripts" "$src/server.py" "$app/"
  for module in voice.py settings.py; do
    if [[ -f "$src/$module" ]]; then cp "$src/$module" "$app/"; fi
  done
fi
cat > "$bin/nanocodex-wow" <<'LAUNCHER'
#!/usr/bin/env bash
exec /usr/bin/python3 "$HOME/.local/share/nanocodex-wow/scripts/launch.py" "$@"
LAUNCHER
chmod +x "$bin/nanocodex-wow"
cat > "$HOME/.config/systemd/user/nanocodex-wow.service" <<SERVICE
[Unit]
Description=Nanocodex WoW companion
After=network.target
[Service]
Type=simple
WorkingDirectory=$app
ExecStart=/usr/bin/python3 $app/server.py
Restart=on-failure
RestartSec=3
UMask=0077
[Install]
WantedBy=default.target
SERVICE
cat > "$HOME/.local/share/applications/nanocodex-wow.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Nanocodex WoW
Comment=Agents, voice and your Azeroth companion
Exec=$bin/nanocodex-wow
Icon=applications-games
Categories=Game;Utility;
Terminal=false
DESKTOP
systemctl --user daemon-reload
systemctl --user enable --now nanocodex-wow.service
systemctl --user restart nanocodex-wow.service
# Install a dedicated fragment in the configuration format actually in use.
if command -v hyprctl >/dev/null && hyprctl binds -j >/dev/null 2>&1; then
  python3 - "$bin/nanocodex-wow" <<'PYBINDS'
import json, os, pathlib, shlex, shutil, subprocess, sys, time
root = pathlib.Path.home() / '.config/hypr'
root.mkdir(parents=True, exist_ok=True)
launcher = sys.argv[1]
rows = json.loads(subprocess.check_output(['hyprctl', 'binds', '-j']))
owned = 'Nanocodex WoW '
selected = []
for key, mode in [('N','home'),('P','projects'),('V','voice')]:
    conflicts = [r for r in rows if r.get('modmask') == 9 and r.get('key','').upper() == key
                 and not str(r.get('description','')).startswith(owned)]
    if conflicts:
        print(f'Alt+Shift+{key} already assigned; preserved.')
    else:
        selected.append((key, mode))
version = json.loads(subprocess.check_output(['hyprctl', 'version', '-j']))
import re
match = re.search(r'(\d+)\.(\d+)', str(version.get('version') or version.get('tag') or ''))
modern = (root / 'hyprland.lua').exists() or bool(match and tuple(map(int,match.groups())) >= (0,55))
main = root / ('hyprland.lua' if modern else 'hyprland.conf')
if not main.exists():
    raise SystemExit(f'Cannot find active Hyprland config {main}; no bindings changed.')
fragment = root / ('nanocodex-wow.lua' if modern else 'nanocodex-wow.conf')
if modern:
    content = '-- Managed by Nanocodex WoW installer; user shortcuts are preserved.\n'
    for key,mode in selected:
        command = shlex.quote(launcher) + ' ' + mode
        content += f'hl.bind("ALT + SHIFT + {key}", hl.dsp.exec_cmd({json.dumps(command)}), {{ description = "{owned}{mode}" }})\n'
    source = 'dofile(' + json.dumps(str(fragment)) + ') -- Nanocodex WoW shortcuts'
else:
    content = '# Managed by Nanocodex WoW installer\n' + ''.join(f'bindd = ALT SHIFT, {key}, {owned}{mode}, exec, {shlex.quote(launcher)} {mode}\n' for key,mode in selected)
    source = f'source = {fragment}'
old = main.read_text()
# Only remove exact lines produced by the previous installer, never other user bindings.
legacy = root / 'bindings.conf'
if legacy.exists():
    previous = legacy.read_text()
    generated = {f'bindd = ALT SHIFT, {key}, {owned}{mode}, exec, {launcher} {mode}' for key,mode in [('N','home'),('P','projects'),('V','voice')]}
    cleaned = ''.join(line for line in previous.splitlines(keepends=True) if line.rstrip('\n') not in generated)
    if cleaned != previous:
        shutil.copy2(legacy, str(legacy) + '.nanocodex-wow-backup-' + str(time.time_ns()))
        legacy.write_text(cleaned)
fragment.write_text(content)
if source not in old.splitlines():
    shutil.copy2(main, str(main) + '.nanocodex-wow-backup-' + str(time.time_ns()))
    main.write_text(old.rstrip() + '\n\n' + source + '\n')
subprocess.run(['hyprctl', 'reload'], check=True)
print(f'Installed {len(selected)} shortcuts using {fragment.name}.')
PYBINDS
fi
printf 'Installed. Open: %s/nanocodex-wow\n' "$bin"
printf 'Sign in through the companion or: /opt/nanocodex/current/nanocodex2 login\n'
