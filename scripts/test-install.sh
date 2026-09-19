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
  "nanocodex-computer-x86_64-unknown-linux-gnu"
)
binary_sources=(
  "$temporary_root/${binary_names[0]}"
  "$temporary_root/${binary_names[1]}"
  "$temporary_root/${binary_names[2]}"
)
printf '%s\n' '#!/bin/sh' 'printf "%s\n" "nanocodex 1.2.3"' > "${binary_sources[0]}"
printf '%s\n' '#!/bin/sh' 'printf "%s\n" "nanocodex2 1.2.3"' > "${binary_sources[1]}"
printf '%s\n' '#!/bin/sh' 'printf "%s\n" "nanocodex-computer 0.1.0"' > "${binary_sources[2]}"
chmod +x "${binary_sources[@]}"

voice_asset="nanocodex-voice-x86_64-unknown-linux-gnu.tar.gz"
make_voice_fixture() {
  python3 - "$1/$voice_asset" "${2:-valid}" <<'PY'
import io, sys, tarfile
with tarfile.open(sys.argv[1], 'w:gz', format=tarfile.USTAR_FORMAT) as archive:
    names = ['bin/nanocodex-voice-host', 'runtime.json', 'manifest.json', 'sources.json', 'NOTICE.md', 'lib/libgstreamer-1.0.so.0', 'licenses/LGPL-2.1.txt']
    if sys.argv[2] == 'static': names[names.index('lib/libgstreamer-1.0.so.0')] = 'libwebrtc.json'
    if sys.argv[2] == 'incomplete': names.remove('bin/nanocodex-voice-host')
    for name in names:
        entry = tarfile.TarInfo('nanocodex-resources/voice/' + name)
        data = b'fixture voice runtime\n'
        entry.size = len(data); entry.mode = 0o755 if '/bin/' in entry.name else 0o644
        archive.addfile(entry, io.BytesIO(data))
    if sys.argv[2] in ['traversal', 'link']:
        entry = tarfile.TarInfo('nanocodex-resources/voice/../../escape' if sys.argv[2] == 'traversal' else 'nanocodex-resources/voice/link')
        if sys.argv[2] == 'link': entry.type = tarfile.SYMTYPE; entry.linkname = '/tmp'
        archive.addfile(entry)
PY
  printf '%s  %s\n' "$(sha256_file "$1/$voice_asset")" "$voice_asset" >> "$1/SHA256SUMS"
}

run_case() {
  local format="$1"
  local case_root="$temporary_root/$format-${2:-valid}"
  local fixture="$case_root/fixture"
  local marker="$case_root/profile-injection"
  local install_root="$case_root/install '\$(touch $marker)'"
  local asset digest output index

  mkdir -p "$fixture" "$case_root/home"
  : > "$fixture/SHA256SUMS"
  for index in "${!binary_names[@]}"; do
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
  make_voice_fixture "$fixture" "${2:-valid}"

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
  [[ "$("$install_root/bin/nanocodex-computer" --version)" == 'nanocodex-computer 0.1.0' ]]
  [[ -f "$install_root/updater/nanocodex.sha256" ]]
  [[ -f "$install_root/versions/1.2.3/nanocodex.sha256" ]]
  [[ -f "$install_root/versions/1.2.3/nanocodex2.sha256" ]]
  [[ -x "$install_root/current/nanocodex-resources/voice/bin/nanocodex-voice-host" ]]

  rm "$install_root/current/nanocodex-resources/voice/bin/nanocodex-voice-host"
  PATH="$mock_bin:$PATH" HOME="$case_root/home" SHELL=/bin/bash NANOCODEX_DIR="$install_root" \
    NANOCODEX_INSTALL_FIXTURE="$fixture" bash "$workspace_root/install" >/dev/null
  [[ -x "$install_root/current/nanocodex-resources/voice/bin/nanocodex-voice-host" ]]
  [[ -f "$install_root/current/nanocodex-voice.sha256" ]]
  [[ "$(cat "$install_root/current/nanocodex-voice.archive.sha256")" == "$(sha256_file "$fixture/$voice_asset")" ]]

  # Repair the exact older binary bundle that originally omitted voice.
  rm -r "$install_root/current/nanocodex-resources"
  PATH="$mock_bin:$PATH" HOME="$case_root/home" SHELL=/bin/bash NANOCODEX_DIR="$install_root" \
    NANOCODEX_INSTALL_FIXTURE="$fixture" bash "$workspace_root/install" >/dev/null
  [[ -x "$install_root/current/nanocodex-resources/voice/bin/nanocodex-voice-host" ]]

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
  for index in "${!binary_names[@]}"; do
    asset="${binary_names[$index]}"
    cp "${binary_sources[$index]}" "$fixture/$asset"
    digest="$(sha256_file "$fixture/$asset")"
    if [[ "$failure" == missing-main-checksum && "$index" == 0 ]] || \
      [[ "$failure" == missing-companion-checksum && "$index" == 1 ]]; then
      continue
    fi
    if [[ "$failure" == invalid-main-checksum && "$index" == 0 ]] || \
      [[ "$failure" == invalid-companion-checksum && "$index" == 1 ]] || \
      [[ "$failure" == invalid-computer-checksum && "$index" == 2 ]]; then
      digest=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
    fi
    printf '%s  %s\n' "$digest" "$asset" >> "$fixture/SHA256SUMS"
  done
  if [[ "$failure" == voice-* ]]; then
    make_voice_fixture "$fixture" "${failure#voice-}"
    if [[ "$failure" == voice-checksum ]]; then printf 'corrupt' >> "$fixture/$voice_asset"; fi
    if [[ "$failure" == voice-missing ]]; then rm "$fixture/$voice_asset"; fi
    mkdir -p "$install_root/versions/previous"
    printf 'previous installation' > "$install_root/versions/previous/marker"
    ln -s versions/previous "$install_root/current"
  fi

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
  if [[ "$failure" == voice-* ]]; then
    [[ "$(readlink "$install_root/current")" == versions/previous ]]
    [[ "$(cat "$install_root/current/marker")" == 'previous installation' ]]
  else
    [[ ! -e "$install_root/current" ]]
  fi
  [[ ! -e "$install_root/bin/nanocodex" ]]
  [[ ! -e "$install_root/bin/nanocodex2" ]]
  [[ ! -e "$install_root/bin/nanocodex-computer" ]]
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
    invalid-computer-checksum)
      grep -Fq 'checksum mismatch for nanocodex-computer-x86_64-unknown-linux-gnu' <<<"$output"
      ;;
    voice-*) ;;
  esac
}

run_case raw
run_case gzip
run_case raw static
run_rejected_case missing-main-checksum
run_rejected_case missing-companion-checksum
run_rejected_case invalid-main-checksum
run_rejected_case invalid-companion-checksum
run_rejected_case invalid-computer-checksum
run_rejected_case voice-checksum
run_rejected_case voice-missing
run_rejected_case voice-incomplete
run_rejected_case voice-traversal
run_rejected_case voice-link

echo "installer verifies, installs and repairs voice alongside raw and gzip binary bundles"

# macOS invokes the installed native setup helper with the same custom root.
# These checksummed release fixtures are shell stubs; no upstream download occurs.
run_mac_cua_case() {
  local mode="$1" case_root="$temporary_root/mac-cua-$1"
  local fixture="$case_root/fixture" install_root="$case_root/install with spaces"
  local output status=0 setting="" helper_exit=0 help_exit=0
  mkdir -p "$fixture" "$case_root/home"
  printf '%s\n' '#!/bin/sh' 'exit 0' > "$fixture/nanocodex-aarch64-apple-darwin"
  cat > "$fixture/nanocodex2-aarch64-apple-darwin" <<'HELPER'
#!/bin/sh
if [ "${3-}" = --help ]; then exit "${CUA_HELP_EXIT:-0}"; fi
printf '%s\n' "$NANOCODEX_DIR" "$@" > "$CUA_SETUP_RECORD"
exit "$CUA_SETUP_EXIT"
HELPER
  : > "$fixture/SHA256SUMS"
  for name in nanocodex-aarch64-apple-darwin nanocodex2-aarch64-apple-darwin; do
    printf '%s  %s\n' "$(sha256_file "$fixture/$name")" "$name" >> "$fixture/SHA256SUMS"
  done
  case "$mode" in off) setting=off ;; explicit) setting=/custom/provider ;; failure) helper_exit=1 ;; old) help_exit=2 ;; esac
  output="$(PATH="$mock_bin:$PATH" HOME="$case_root/home" SHELL=/bin/bash \
    TEST_INSTALL_OS=Darwin TEST_INSTALL_ARCH=arm64 NANOCODEX_DIR="$install_root" \
    NANOCODEX_COMPUTER="$setting" NANOCODEX_INSTALL_FIXTURE="$fixture" \
    CUA_SETUP_RECORD="$case_root/setup" CUA_SETUP_EXIT="$helper_exit" CUA_HELP_EXIT="$help_exit" \
    bash "$workspace_root/install" 2>&1)" || status=$?
  [[ -x "$install_root/current/nanocodex2" ]]
  if [[ "$mode" == off || "$mode" == explicit || "$mode" == old ]]; then
    [[ "$status" == 0 && ! -e "$case_root/setup" ]]
  else
    [[ "$(cat "$case_root/setup")" == "$install_root"$'\ncomputer\nsetup\n--refresh' ]]
    if [[ "$mode" == failure ]]; then
      [[ "$status" != 0 ]]
      grep -Fq 'CLIs installed, but upstream CUA setup failed' <<<"$output"
    else
      [[ "$status" == 0 ]]
    fi
  fi
}
run_mac_cua_case success
run_mac_cua_case failure
run_mac_cua_case off
run_mac_cua_case explicit
run_mac_cua_case old
