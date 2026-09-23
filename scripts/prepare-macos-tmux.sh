#!/bin/bash
# Prepare a private tmux executable without altering its source or any server.
set -euo pipefail

if [[ $# != 2 || $(uname -s) != Darwin ]]; then
  echo "Usage (macOS): $0 /path/to/source/tmux /new/path/to/tmux" >&2
  exit 2
fi
source_tmux=$1
destination=$2
if [[ ! -f "$source_tmux" || ! -x "$source_tmux" ]]; then
  echo "Source must be an executable tmux file." >&2
  exit 2
fi
if [[ -e "$destination" || -L "$destination" ]]; then
  echo "Destination already exists; choose a new path." >&2
  exit 2
fi

# Restrict this workaround to the ad-hoc executables it was measured with. Do
# not replace a vendor signature or change hardened-runtime policy/entitlements.
metadata=$(/usr/bin/codesign --display --verbose=2 "$source_tmux" 2>&1)
case "$metadata" in
  *'flags=0x20002(adhoc,linker-signed)'*|*'flags=0x2(adhoc)'*) ;;
  *) echo "Source is not a supported ad-hoc tmux executable." >&2; exit 2 ;;
esac
identifier=$(printf '%s\n' "$metadata" | /usr/bin/sed -n 's/^Identifier=//p')
if [[ "$identifier" != tmux ]]; then
  echo "Source signature does not identify tmux." >&2
  exit 2
fi
entitlements=$(/usr/bin/codesign --display --entitlements :- "$source_tmux" 2>/dev/null)
if [[ -n "$entitlements" ]]; then
  echo "Source has entitlements; refusing to replace its signing metadata." >&2
  exit 2
fi
/usr/bin/codesign --verify --strict "$source_tmux"

parent=$(dirname "$destination")
mkdir -p "$parent"
staged=$(mktemp "$parent/.tmux-prepared.XXXXXX")
trap 'rm -f "$staged"' EXIT
cp "$source_tmux" "$staged"
chmod u+x "$staged"
/usr/bin/codesign --force --sign - --identifier tmux "$staged"
/usr/bin/codesign --verify --strict "$staged"
# Publish without overwriting an existing file or following a destination symlink.
/bin/ln -h "$staged" "$destination"
printf 'Prepared %s\n' "$destination"
printf 'Existing tmux servers were not changed. To start a separate server:\n  '
printf '%q ' "$destination" -L nanocodex-fast new-session
printf '\n'
