#!/usr/bin/env bash
set -euo pipefail

SJ_PROJECT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
SJ_VENV=${SJ_VENV:-${VIRTUAL_ENV:-${HOME}/.venvs/dev}}
SJ_TOOLCHAIN="$SJ_VENV/opt/rust-1.99.0"
SJ_INSTALL_DIR=${SJ_INSTALL_DIR:-${HOME}/local/bin}
export PATH="$SJ_VENV/bin:$SJ_TOOLCHAIN/lib/rustlib/aarch64-unknown-linux-musl/bin:$PATH"
export CARGO_HOME="$SJ_VENV/share/cargo"
SJ_TARGET_DIR=$(mktemp -d /tmp/sj-build.XXXXXX)
export CARGO_TARGET_DIR="$SJ_TARGET_DIR"
export SJ_PROJECT SJ_VENV SJ_TOOLCHAIN SJ_INSTALL_DIR
trap 'rm -rf -- "$SJ_TARGET_DIR"' EXIT
mkdir -p "$SJ_PROJECT/.logs"
exec > >("$SJ_VENV/bin/python" -u "$SJ_PROJECT/scripts/sanitize_log.py" | tee "$SJ_PROJECT/.logs/build.log") 2>&1
cd "$SJ_PROJECT"
test -x "$SJ_VENV/bin/cargo"

# Keep personal source/cache paths out of compiler diagnostics and the binary.
SJ_RUSTFLAGS=(
    -C link-self-contained=yes -C target-feature=+crt-static
    "--remap-path-prefix=${HOME}=/home/user"
    "--remap-path-prefix=$SJ_VENV=/tooling/venv"
    "--remap-path-prefix=$CARGO_HOME=/tooling/cargo"
    "--remap-path-prefix=$SJ_TOOLCHAIN=/tooling/rust"
    "--remap-path-prefix=$SJ_PROJECT=/workspace/sj"
    "--remap-path-prefix=$SJ_TARGET_DIR=/build/sj"
)
printf -v CARGO_ENCODED_RUSTFLAGS '%s\x1f' "${SJ_RUSTFLAGS[@]}"
export CARGO_ENCODED_RUSTFLAGS=${CARGO_ENCODED_RUSTFLAGS%$'\x1f'}
rustc --version
cargo --version
bash -n build.sh setup-toolchain.sh
"$SJ_VENV/bin/python" -B -m unittest discover -s tests -p 'test_*.py' -v
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
SJ_BINARY="$SJ_TARGET_DIR/aarch64-unknown-linux-musl/release/sj"
file "$SJ_BINARY"
file "$SJ_BINARY" | rg -q 'statically linked|static-pie linked'
if ldd "$SJ_BINARY" 2>&1 | "$SJ_VENV/bin/python" -u "$SJ_PROJECT/scripts/sanitize_log.py" | tee "$SJ_PROJECT/.logs/linkage.log" | rg -q 'lib.*=>'; then
    printf 'Refusing to install a binary with shared-library dependencies.\n' >&2
    exit 1
fi
"$SJ_BINARY" --version
mkdir -p "$SJ_INSTALL_DIR"
install -m 755 "$SJ_BINARY" "$SJ_INSTALL_DIR/.sj.installing"
mv -f -- "$SJ_INSTALL_DIR/.sj.installing" "$SJ_INSTALL_DIR/sj"
printf 'Installed %s/sj\n' "$SJ_INSTALL_DIR"
