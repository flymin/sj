#!/usr/bin/env bash
set -euo pipefail

SJ_PROJECT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
SJ_PYTHON=${SJ_PYTHON:-python3}
SJ_TARGET=${SJ_TARGET:?Set SJ_TARGET to a supported Linux musl target}
case "$SJ_TARGET" in
    x86_64-unknown-linux-musl) SJ_ARCH=x86_64 ;;
    aarch64-unknown-linux-musl) SJ_ARCH=aarch64 ;;
    *) printf 'Unsupported release target: %s\n' "$SJ_TARGET" >&2; exit 1 ;;
esac
test "$(uname -m)" = "$SJ_ARCH"
SJ_TOOLCHAIN=$(rustc --print sysroot)
SJ_HOST=$(rustc -vV | sed -n 's/^host: //p')
export PATH="$SJ_TOOLCHAIN/lib/rustlib/$SJ_HOST/bin:$PATH"
export CARGO_HOME=${CARGO_HOME:-${HOME}/.cargo}
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$SJ_PROJECT/target/ci}
SJ_DIST_DIR=${SJ_DIST_DIR:-$SJ_PROJECT/dist}
export SJ_PROJECT SJ_TOOLCHAIN SJ_DIST_DIR
mkdir -p "$SJ_PROJECT/.logs"
exec > >("$SJ_PYTHON" -u "$SJ_PROJECT/scripts/sanitize_log.py" | tee "$SJ_PROJECT/.logs/ci-build.log") 2>&1
cd "$SJ_PROJECT"
trap 'printf "::error::CI failed at line %s: %s\n" "$LINENO" "$BASH_COMMAND"' ERR

SJ_RUSTFLAGS=(
    -C link-self-contained=yes -C target-feature=+crt-static
    "--remap-path-prefix=${HOME}=/home/user"
    "--remap-path-prefix=$CARGO_HOME=/tooling/cargo"
    "--remap-path-prefix=$SJ_TOOLCHAIN=/tooling/rust"
    "--remap-path-prefix=$SJ_PROJECT=/workspace/sj"
    "--remap-path-prefix=$CARGO_TARGET_DIR=/build/sj"
)
printf -v CARGO_ENCODED_RUSTFLAGS '%s\x1f' "${SJ_RUSTFLAGS[@]}"
export CARGO_ENCODED_RUSTFLAGS=${CARGO_ENCODED_RUSTFLAGS%$'\x1f'}
"$SJ_PYTHON" -B scripts/release.py metadata
rustc --version
bash -n build.sh setup-toolchain.sh scripts/*.sh
"$SJ_PYTHON" -B -m unittest discover -s tests -p 'test_*.py' -v
cargo fmt --check
cargo test --locked --target "$SJ_TARGET"
cargo clippy --locked --target "$SJ_TARGET" --all-targets -- -D warnings
cargo build --locked --release --target "$SJ_TARGET"

SJ_BINARY="$CARGO_TARGET_DIR/$SJ_TARGET/release/sj"
file "$SJ_BINARY"
"$SJ_PYTHON" -B scripts/release.py verify --binary "$SJ_BINARY" --target "$SJ_TARGET"
SJ_VERSION=$("$SJ_PYTHON" -B scripts/release.py version)
test "$("$SJ_BINARY" --version)" = "sj $SJ_VERSION"
for SJ_COMMAND in q out script sh; do
    "$SJ_BINARY" "$SJ_COMMAND" --help >/dev/null
done
"$SJ_PYTHON" -B scripts/release.py package --binary "$SJ_BINARY" --target "$SJ_TARGET" --destination "$SJ_DIST_DIR"
