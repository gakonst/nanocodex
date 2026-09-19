#!/usr/bin/env bash
# Install in the current gaming desktop user's session. No privilege switching.
set -euo pipefail
source_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
client=${1:-"$HOME/Games/battlenet/drive_c/Program Files (x86)/World of Warcraft/_classic_beta_"}
if [[ ! -d "$client/Interface" ]]; then
  echo 'Pass the running WoW client directory containing Interface.' >&2
  exit 2
fi
if [[ $(id -u) == 0 || -z ${XDG_RUNTIME_DIR:-} || ! -O "$XDG_RUNTIME_DIR" ]]; then
  echo 'Run from the logged-in gaming desktop user terminal.' >&2
  exit 1
fi
# An already running bridge may have an accepted request. Do not replace its
# code or restart it from this installer; preserve its journals for reconciliation.
if systemctl --user is-active --quiet nanocodex-wow-bridge.service; then
  echo 'The WoW bridge is already running. Finish its current request and stop it before updating.' >&2
  exit 1
fi
bash "$source_root/scripts/install-desktop.sh"
bash "$source_root/transport/install.sh"
bash "$source_root/scripts/install-addon.sh" "$client"
bash "$source_root/transport/install-autoconnect.sh" --defer-start
printf '\nUpdate installed. In WoW, enter /reload once. Then return here and press Enter.\n'
if [[ ! -t 0 ]]; then
  printf 'Connection has not started. After /reload: systemctl --user start nanocodex-wow-bridge.service\n'
  exit 2
fi
read -r
systemctl --user start nanocodex-wow-bridge.service
"$HOME/.local/bin/nanocodex-wow" home
printf 'Companion opened. Return to WoW; connection discovery will begin when the game is in front.\n'
