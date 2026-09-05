#!/usr/bin/env bash
set -euo pipefail

workspace_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
temporary_root="$(mktemp -d 2>/dev/null)" || {
  echo "test-install: failed to create a temporary directory" >&2
  exit 1
}
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
  -s) printf '%s\n' Linux ;;
  -m) printf '%s\n' x86_64 ;;
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
    --head)
      head_request=true
      shift
      ;;
    --output)
      output="$2"
      shift 2
      ;;
    --write-out)
      shift 2
      ;;
    http://*|https://*)
      url="$1"
      shift
      ;;
    *)
      shift
      ;;
  esac
done

if [[ "$head_request" == true ]]; then
  printf '%s\n' 'https://github.com/gakonst/nanocodex/releases/tag/v1.2.3'
  exit 0
fi

asset="${url##*/}"
if [[ -z "$output" || ! -f "$NANOCODEX_INSTALL_FIXTURE/$asset" ]]; then
  exit 22
fi
cp "$NANOCODEX_INSTALL_FIXTURE/$asset" "$output"
EOF
chmod +x "$mock_bin/uname" "$mock_bin/curl"

binary_names=(
  "nanocodex-x86_64-unknown-linux-gnu"
  "nanocodex2-x86_64-unknown-linux-gnu"
)
binary_sources=(
  "$temporary_root/${binary_names[0]}"
  "$temporary_root/${binary_names[1]}"
)
printf '%s\n' '#!/bin/sh' 'printf "%s\n" "nanocodex 1.2.3"' > "${binary_sources[0]}"
printf '%s\n' '#!/bin/sh' 'printf "%s\n" "nanocodex2 1.2.3"' > "${binary_sources[1]}"
chmod +x "${binary_sources[@]}"

run_case() {
  local format="$1"
  local case_root="$temporary_root/$format"
  local fixture="$case_root/fixture"
  local marker="$case_root/profile-injection"
  local install_root="$case_root/install '\$(touch $marker)'"
  local asset digest output index

  mkdir -p "$fixture" "$case_root/home"
  : > "$fixture/SHA256SUMS"
  for index in 0 1; do
    if [[ "$format" == gzip ]]; then
      asset="${binary_names[$index]}.gz"
      gzip -n -9 -c "${binary_sources[$index]}" > "$fixture/$asset"
    else
      asset="${binary_names[$index]}"
      cp "${binary_sources[$index]}" "$fixture/$asset"
    fi
    digest="$(sha256_file "$fixture/$asset")"
    printf '%s  %s\n' "$digest" "$asset" >> "$fixture/SHA256SUMS"
  done

  output="$(
    PATH="$mock_bin:$PATH" \
      HOME="$case_root/home" \
      SHELL=/bin/bash \
      NANOCODEX_DIR="$install_root" \
      NANOCODEX_INSTALL_FIXTURE="$fixture" \
      bash "$workspace_root/install"
  )"
  grep -Fq 'Installed nanocodex 1.2.3' <<<"$output"
  grep -Fq 'Installed nanocodex2 1.2.3' <<<"$output"
  [[ "$("$install_root/bin/nanocodex" --version)" == 'nanocodex 1.2.3' ]]
  [[ "$("$install_root/bin/nanocodex2" --version)" == 'nanocodex2 1.2.3' ]]
  [[ -f "$install_root/updater/nanocodex.sha256" ]]
  [[ -f "$install_root/versions/1.2.3/nanocodex.sha256" ]]
  [[ -f "$install_root/versions/1.2.3/nanocodex2.sha256" ]]

  PATH=/usr/bin:/bin bash "$case_root/home/.bashrc"
  [[ ! -e "$marker" ]]
}

run_rejected_case() {
  local failure="$1"
  local case_root="$temporary_root/rejected-$failure"
  local fixture="$case_root/fixture"
  local install_root="$case_root/install"
  local asset digest index output

  mkdir -p "$fixture" "$case_root/home"
  : > "$fixture/SHA256SUMS"
  for index in 0 1; do
    asset="${binary_names[$index]}"
    cp "${binary_sources[$index]}" "$fixture/$asset"
    digest="$(sha256_file "$fixture/$asset")"
    if [[ "$failure" == missing-main-checksum && "$index" == 0 ]] || \
      [[ "$failure" == missing-companion-checksum && "$index" == 1 ]]; then
      continue
    fi
    if [[ "$failure" == invalid-main-checksum && "$index" == 0 ]] || \
      [[ "$failure" == invalid-companion-checksum && "$index" == 1 ]]; then
      digest=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
    fi
    printf '%s  %s\n' "$digest" "$asset" >> "$fixture/SHA256SUMS"
  done

  if output="$(
    PATH="$mock_bin:$PATH" \
      HOME="$case_root/home" \
      SHELL=/bin/bash \
      NANOCODEX_DIR="$install_root" \
      NANOCODEX_INSTALL_FIXTURE="$fixture" \
      bash "$workspace_root/install" 2>&1
  )"; then
    echo "test-install: installer accepted $failure" >&2
    exit 1
  fi
  [[ ! -e "$install_root/current" ]]
  [[ ! -e "$install_root/bin/nanocodex" ]]
  [[ ! -e "$install_root/bin/nanocodex2" ]]
  case "$failure" in
    missing-main-checksum)
      grep -Fq 'contains neither nanocodex-x86_64-unknown-linux-gnu.gz nor nanocodex-x86_64-unknown-linux-gnu' <<<"$output"
      ;;
    missing-companion-checksum)
      grep -Fq 'contains neither nanocodex2-x86_64-unknown-linux-gnu.gz nor nanocodex2-x86_64-unknown-linux-gnu' <<<"$output"
      ;;
    invalid-main-checksum)
      grep -Fq 'checksum mismatch for nanocodex-x86_64-unknown-linux-gnu' <<<"$output"
      ;;
    invalid-companion-checksum)
      grep -Fq 'checksum mismatch for nanocodex2-x86_64-unknown-linux-gnu' <<<"$output"
      ;;
  esac
}

run_case raw
run_case gzip
run_rejected_case missing-main-checksum
run_rejected_case missing-companion-checksum
run_rejected_case invalid-main-checksum
run_rejected_case invalid-companion-checksum

echo "installer atomically activates verified raw and gzip bundles for both binaries"
