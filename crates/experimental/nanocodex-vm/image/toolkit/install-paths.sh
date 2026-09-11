#!/bin/sh
# ext4 does not retain OCI ENV, and login shells reset PATH. Expose the optional
# toolchains through the standard Linux executable path in both environments.
set -eu
mkdir -p /etc/profile.d
cat > /etc/profile.d/nanocodex-toolkit.sh <<'PROFILE'
export RUSTUP_HOME="${RUSTUP_HOME:-/opt/rustup}"
export PATH="/opt/hand-python/bin:/opt/node/bin:/opt/cargo/bin:/opt/swift/usr/bin:/usr/local/go/bin:$PATH"
PROFILE
for directory in /opt/hand-python/bin /opt/node/bin /opt/cargo/bin /opt/swift/usr/bin /usr/local/go/bin; do
    for command in python python3 pip pip3 pytest node npm npx pnpm pnpx cargo rustc rustdoc rustup rustfmt cargo-fmt cargo-clippy clippy-driver swift swiftc swift-driver swift-frontend swift-package swift-build swift-test swift-run sourcekit-lsp go gofmt; do
        test -x "$directory/$command" || continue
        # Remove a previous symlink before writing so its target is never changed.
        rm -f "/usr/local/bin/$command"
        {
            printf '#!/bin/sh\n'
            printf 'export RUSTUP_HOME="${RUSTUP_HOME:-/opt/rustup}"\n'
            printf 'export PATH="/opt/hand-python/bin:/opt/node/bin:/opt/cargo/bin:/opt/swift/usr/bin:/usr/local/go/bin:$PATH"\n'
            printf 'exec "%s/%s" "$@"\n' "$directory" "$command"
        } > "/usr/local/bin/$command"
        chmod 0755 "/usr/local/bin/$command"
    done
done
