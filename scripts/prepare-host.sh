#!/usr/bin/env bash
# Source after selecting the toolchain and CARGO_TARGET_DIR. A musl compiler
# needs shared libraries for build-time procedural macros, even for a static CLI.
# Reuse existing toolchain libraries in a temporary search directory.
SJ_HOST=$(rustc -vV | sed -n 's/^host: //p')
SJ_REAL_LLD=$(command -v rust-lld)
SJ_HOST_GCC="$SJ_TOOLCHAIN/lib/libgcc_s.so.1"
SJ_HOST_LIBC="$SJ_VENV/opt/musl/usr/lib/${SJ_HOST%%-*}-linux-musl/libc.so"
if [[ "$SJ_HOST" == *-linux-musl && -f "$SJ_HOST_GCC" && -f "$SJ_HOST_LIBC" ]]; then
    SJ_HOST_LIB="$CARGO_TARGET_DIR/.host/lib"
    mkdir -p "$SJ_HOST_LIB" "$CARGO_TARGET_DIR/.host/bin"
    ln -sf "$SJ_HOST_GCC" "$SJ_HOST_LIB/libgcc_s.so"
    ln -sf "$SJ_HOST_LIBC" "$SJ_HOST_LIB/libc.so"
    cat > "$CARGO_TARGET_DIR/.host/bin/rust-lld" <<'EOF'
#!/usr/bin/env bash
for sj_arg in "$@"; do
    if [[ "$sj_arg" == -shared ]]; then
        exec "$SJ_REAL_LLD" "$@" "-L$SJ_HOST_LIB"
    fi
done
exec "$SJ_REAL_LLD" "$@"
EOF
    chmod +x "$CARGO_TARGET_DIR/.host/bin/rust-lld"
    export SJ_REAL_LLD SJ_HOST_LIB
    export PATH="$CARGO_TARGET_DIR/.host/bin:$PATH"
    SJ_HOST_KEY=${SJ_HOST^^}
    SJ_HOST_KEY=${SJ_HOST_KEY//-/_}
    export "CARGO_TARGET_${SJ_HOST_KEY}_LINKER=$CARGO_TARGET_DIR/.host/bin/rust-lld"
fi
