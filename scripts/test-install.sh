#!/usr/bin/env bash
set -euo pipefail

workspace_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
temporary_root="$(mktemp -d 2>/dev/null)"
trap 'rm -rf -- "$temporary_root"' EXIT

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  else
    shasum -a 256 "$1" | awk '{ print $1 }'
  fi
}

mock_bin="$temporary_root/mock-bin"
mkdir -p "$mock_bin"
cat > "$mock_bin/uname" <<'EOF'
#!/bin/sh
case "${1-}" in
  -s) printf '%s\n' "${TEST_INSTALL_OS:-Linux}" ;;
  -m) printf '%s\n' "${TEST_INSTALL_ARCH:-x86_64}" ;;
  *) exit 2 ;;
esac
EOF
cat > "$mock_bin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
head_request=false
output=""
url=""
while (($#)); do
  case "$1" in
    --head) head_request=true; shift ;;
    --output) output="$2"; shift 2 ;;
    --write-out) shift 2 ;;
    http://*|https://*) url="$1"; shift ;;
    *) shift ;;
  esac
done
if [[ "$head_request" == true ]]; then
  printf '%s' 'https://github.com/gakonst/nanocodex/releases/tag/v1.2.3'
  exit 0
fi
printf '%s\n' "${url##*/}" >> "$TEST_INSTALL_DOWNLOADS"
printf '%s\n' "$url" >> "$TEST_INSTALL_URLS"
asset="${url##*/}"
[[ -n "$output" && -f "$TEST_INSTALL_FIXTURE/$asset" ]]
cp "$TEST_INSTALL_FIXTURE/$asset" "$output"
EOF
chmod +x "$mock_bin/uname" "$mock_bin/curl"

make_bootstrap() {
  local path="$1"
  cat > "$path" <<'EOF'
#!/bin/sh
printf '%s\n' "$NANOCODEX_DIR" "$@" > "$TEST_INSTALL_RECORD"
EOF
  chmod +x "$path"
}

make_stable_installer() {
  local path="$1"
  cat > "$path" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
bash_only=(tagged installer)
[[ "${bash_only[*]}" == "tagged installer" ]]
exec sh "$TEST_INSTALL_CURRENT_INSTALL" "$@"
EOF
  chmod +x "$path"
}

run_case() {
  local format="$1" os="${2:-Linux}" arch="${3:-x86_64}"
  local case_root="$temporary_root/$format-$os-$arch"
  local fixture="$case_root/fixture" downloads="$case_root/downloads"
  local target binary asset source
  mkdir -p "$fixture"
  case "$os-$arch" in
    Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
    Darwin-arm64) target=aarch64-apple-darwin ;;
    *) return 2 ;;
  esac
  binary="nanocodex-$target"
  source="$case_root/$binary"
  make_bootstrap "$source"
  if [[ "$format" == gzip ]]; then
    asset="$binary.gz"
    gzip -n -9 -c "$source" > "$fixture/$asset"
  else
    asset="$binary"
    cp "$source" "$fixture/$asset"
  fi
  printf '%s  %s\n' "$(sha256_file "$fixture/$asset")" "$asset" > "$fixture/SHA256SUMS"
  # These entries prove the shell never fetches the companion or voice bundle.
  printf '%064d  nanocodex2-%s\n' 0 "$target" >> "$fixture/SHA256SUMS"
  printf '%064d  nanocodex-voice-%s.tar.gz\n' 0 "$target" >> "$fixture/SHA256SUMS"
  make_stable_installer "$fixture/install"

  PATH="$mock_bin:$PATH" HOME="$case_root/home" NANOCODEX_DIR="$case_root/install" \
    NANOCODEX_INSTALL_NO_SETUP=1 TEST_INSTALL_FIXTURE="$fixture" \
    TEST_INSTALL_OS="$os" TEST_INSTALL_ARCH="$arch" TEST_INSTALL_DOWNLOADS="$downloads" \
    TEST_INSTALL_URLS="$case_root/urls" \
    TEST_INSTALL_CURRENT_INSTALL="$workspace_root/install" \
    TEST_INSTALL_RECORD="$case_root/record" \
    sh "$workspace_root/install" --no-modify-path >/dev/null

  [[ "$(cat "$case_root/record")" == "$case_root/install"$'\ninstall\n--no-modify-path' ]]
  [[ "$(cat "$downloads")" == $'install\nSHA256SUMS\n'"$asset" ]]
  grep -Fxq "https://raw.githubusercontent.com/gakonst/nanocodex/refs/tags/v1.2.3/install" \
    "$case_root/urls"
  grep -Fxq "https://github.com/gakonst/nanocodex/releases/download/v1.2.3/$asset" \
    "$case_root/urls"
}

run_case raw
run_case gzip
run_case raw Darwin arm64

rejected="$temporary_root/rejected"
mkdir -p "$rejected/fixture"
make_bootstrap "$rejected/fixture/nanocodex-x86_64-unknown-linux-gnu"
make_stable_installer "$rejected/fixture/install"
printf '%064d  nanocodex-x86_64-unknown-linux-gnu\n' 0 > "$rejected/fixture/SHA256SUMS"
if PATH="$mock_bin:$PATH" TEST_INSTALL_FIXTURE="$rejected/fixture" \
  TEST_INSTALL_DOWNLOADS="$rejected/downloads" TEST_INSTALL_RECORD="$rejected/record" \
  TEST_INSTALL_URLS="$rejected/urls" \
  TEST_INSTALL_CURRENT_INSTALL="$workspace_root/install" \
  NANOCODEX_INSTALL_NO_SETUP=1 sh "$workspace_root/install" >"$rejected/output" 2>&1; then
  echo "test-install: accepted a corrupt bootstrap" >&2
  exit 1
fi
grep -Fq 'checksum mismatch' "$rejected/output"
[[ ! -e "$rejected/record" ]]

sh -n "$workspace_root/install"
cmp "$workspace_root/install.ps1" "$workspace_root/js/account/public/install.ps1"
echo "installer downloads one verified Rust bootstrap and delegates the product setup"
