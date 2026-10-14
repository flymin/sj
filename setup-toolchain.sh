#!/usr/bin/env bash
# Install only within the selected, existing uv venv; never edit activation/config files.
set -euo pipefail
SJ_PROJECT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
SJ_VENV=${SJ_VENV:-${VIRTUAL_ENV:-${HOME}/.venvs/dev}}
SJ_PREFIX="$SJ_VENV/opt/rust-1.99.0"
SJ_UV=${SJ_UV:-$(command -v uv || true)}
SJ_UV=${SJ_UV:-${HOME}/.local/bin/uv}
SJ_DOWNLOAD=$(mktemp -d /tmp/sj-toolchain.XXXXXX)
export SJ_PROJECT SJ_VENV SJ_PREFIX SJ_UV SJ_DOWNLOAD
trap 'rm -rf -- "$SJ_DOWNLOAD"' EXIT
mkdir -p "$SJ_PROJECT/.logs"
exec > >("$SJ_VENV/bin/python" -u "$SJ_PROJECT/scripts/sanitize_log.py" | tee "$SJ_PROJECT/.logs/toolchain.log") 2>&1
test "$(uname -m)" = aarch64
test -f "$SJ_VENV/pyvenv.cfg"
rg -q '^uv = ' "$SJ_VENV/pyvenv.cfg"
if "$SJ_PREFIX/bin/rustc" --version 2>/dev/null | rg -q '^rustc 1\.99\.0 ' &&
   "$SJ_PREFIX/lib/rustlib/aarch64-unknown-linux-musl/bin/rust-lld" -flavor gnu --version >/dev/null 2>&1; then
    printf 'Rust 1.99.0 is already configured in %s\n' "$SJ_VENV"
    exit 0
fi
"$SJ_UV" pip install --python "$SJ_VENV/bin/python" 'patchelf==0.19.1.0'
mkdir -p "$SJ_PREFIX"
cd "$SJ_DOWNLOAD"
curl -fL --retry 3 -o manifest.toml https://static.rust-lang.org/dist/channel-rust-1.99.0.toml
for SJ_COMPONENT in rustc cargo rust-std rustfmt-preview clippy-preview; do
    SJ_BLOCK=$(awk -v heading="[pkg.$SJ_COMPONENT.target.aarch64-unknown-linux-musl]" '$0 == heading { found=1; next } found && /^\[/ { exit } found { print }' manifest.toml)
    SJ_URL=$(printf '%s\n' "$SJ_BLOCK" | sed -n 's/^xz_url = "\(.*\)"/\1/p')
    SJ_HASH=$(printf '%s\n' "$SJ_BLOCK" | sed -n 's/^xz_hash = "\(.*\)"/\1/p')
    test -n "$SJ_URL"
    SJ_FILE=${SJ_URL##*/}
    curl -fL --retry 3 -o "$SJ_FILE" "$SJ_URL"
    printf '%s  %s\n' "$SJ_HASH" "$SJ_FILE" | sha256sum -c -
    tar -xJf "$SJ_FILE"
    bash "${SJ_FILE%.tar.xz}/install.sh" --prefix="$SJ_PREFIX" --disable-ldconfig
    rm -rf -- "${SJ_FILE%.tar.xz}" "$SJ_FILE"
done
curl -fL --retry 3 -o musl.deb https://ports.ubuntu.com/pool/universe/m/musl/musl_1.2.5-3build1_arm64.deb
printf '%s  musl.deb\n' 5b253a7120571529885635ec6b0895839d8a73a6238aba3730dfa2339989f145 | sha256sum -c -
dpkg-deb -x musl.deb "$SJ_VENV/opt/musl"
SJ_LOADER="$SJ_VENV/opt/musl/usr/lib/aarch64-linux-musl/libc.so"
test -f "$SJ_LOADER"
curl -fL --retry 3 -o libgcc.apk https://dl-cdn.alpinelinux.org/alpine/v3.22/main/aarch64/libgcc-14.2.0-r6.apk
printf '%s  libgcc.apk\n' ba1835eec3ad8a120efd3d5020e561d53553a0513763a08f509e3ce6d4baa9ca | sha256sum -c -
mkdir alpine-libgcc
tar -xzf libgcc.apk -C alpine-libgcc
cp alpine-libgcc/usr/lib/libgcc_s.so.1 "$SJ_PREFIX/lib/"
for SJ_ELF in "$SJ_PREFIX"/bin/* "$SJ_PREFIX"/lib/*.so* "$SJ_PREFIX"/lib/rustlib/aarch64-unknown-linux-musl/bin/* "$SJ_PREFIX"/lib/rustlib/aarch64-unknown-linux-musl/bin/gcc-ld/*; do
    if [ -f "$SJ_ELF" ] && "$SJ_VENV/bin/patchelf" --print-needed "$SJ_ELF" >/dev/null 2>&1; then
        if "$SJ_VENV/bin/patchelf" --print-interpreter "$SJ_ELF" 2>/dev/null | rg -q 'ld-musl'; then
            "$SJ_VENV/bin/patchelf" --set-interpreter "$SJ_LOADER" "$SJ_ELF"
        fi
        "$SJ_VENV/bin/patchelf" --add-rpath "$SJ_PREFIX/lib" "$SJ_ELF"
    fi
done
for SJ_EXE in rustc cargo rustdoc rustfmt cargo-fmt clippy-driver cargo-clippy rust-lld; do
    SJ_REAL="$SJ_PREFIX/bin/$SJ_EXE"
    if [ "$SJ_EXE" = rust-lld ]; then SJ_REAL="$SJ_PREFIX/lib/rustlib/aarch64-unknown-linux-musl/bin/rust-lld"; fi
    test -x "$SJ_REAL"
    if [ -e "$SJ_VENV/bin/$SJ_EXE" ] || [ -L "$SJ_VENV/bin/$SJ_EXE" ]; then
        test "$(readlink "$SJ_VENV/bin/$SJ_EXE")" = "$SJ_REAL"
    else
        ln -s "$SJ_REAL" "$SJ_VENV/bin/$SJ_EXE"
    fi
done
cp manifest.toml "$SJ_PREFIX/release-manifest.toml"
"$SJ_VENV/bin/rustc" --version
"$SJ_VENV/bin/cargo" --version
"$SJ_VENV/bin/rust-lld" -flavor gnu --version
