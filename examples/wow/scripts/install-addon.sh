#!/usr/bin/env bash
set -euo pipefail
src=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
if [[ $# != 1 || ! -d "$1" ]]; then
  echo 'Usage: install-addon.sh "/path/to/World of Warcraft/_retail_" (or your Classic client directory)' >&2
  exit 2
fi
if [[ ! -d "$1/Interface" ]]; then echo 'Expected a WoW client directory containing Interface; refusing ambiguous target.' >&2; exit 2; fi
mkdir -p "$1/Interface/AddOns"
if [[ -e "$1/Interface/AddOns/Nanocodex" ]]; then
  mkdir -p "$1/Interface/NanocodexBackups"
  cp -a "$1/Interface/AddOns/Nanocodex" "$1/Interface/NanocodexBackups/Nanocodex-$(date +%Y%m%d-%H%M%S)"
fi
cp -R "$src/addon/Nanocodex" "$1/Interface/AddOns/"
# The carrier is developed separately but ships inside the same addon namespace.
install -m 644 "$src/addon/Transport.lua" "$1/Interface/AddOns/Nanocodex/Transport.lua"
echo 'Nanocodex addon installed. Enable it at character select; /reload if already in game.'
