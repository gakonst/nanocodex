#!/usr/bin/env bash
# Fail when the browser CDN example or bindings README pin drifts from the
# published js/bindings package version.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
package_json="$root/js/bindings/package.json"
example="$root/examples/browser-cdn/index.html"
readme="$root/js/bindings/README.md"

version="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$package_json")"
pin="nanocodex@${version}"

fail=0
for path in "$example" "$readme"; do
  if ! grep -Fq "$pin" "$path"; then
    echo "error: expected CDN pin \`$pin\` in ${path#"$root"/}" >&2
    fail=1
  fi
done

if [[ "$fail" -ne 0 ]]; then
  echo "error: update examples/browser-cdn/index.html and js/bindings/README.md to pin $pin" >&2
  exit 1
fi

echo "browser-cdn CDN pin matches js/bindings@$version"
