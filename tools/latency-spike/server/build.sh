#!/bin/sh
# Cross-build spike-server.exe for x86_64-pc-windows-msvc from macOS.
#
# Unlike ../../../scripts/check-windows.sh this LINKS, so it needs the SDK import
# libraries as well as the headers. Both come from the same one-time xwin splat that
# script's header documents:
#
#   rustup target add x86_64-pc-windows-msvc
#   brew install xwin
#   xwin --accept-license --temp splat --output ~/.xwin
#   mkdir -p ~/.xwin/bin
#   ln -sf "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-ar ~/.xwin/bin/llvm-lib
#
# The three /libpath arguments are the whole recipe: the CRT, the Win32 um libraries
# (dxgi, d3d11, mfplat, mfuuid, user32 …) and the UCRT. rust-lld speaks link.exe
# syntax for the msvc target, so /libpath is the right spelling, not -L.
#
# On a real Windows box ~/.xwin is absent, the exports are skipped, and the native
# MSVC toolchain finds its own libraries.
#
# Set XWIN_DIR to point at a splat somewhere other than ~/.xwin.
# Extra arguments are passed straight to cargo (e.g. ./build.sh --verbose).
set -eu

XWIN="${XWIN_DIR:-$HOME/.xwin}"
TARGET=x86_64-pc-windows-msvc
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

if [ -d "$XWIN/crt/lib/x86_64" ]; then
    export CC_x86_64_pc_windows_msvc="${CC_x86_64_pc_windows_msvc:-clang}"
    export AR_x86_64_pc_windows_msvc="${AR_x86_64_pc_windows_msvc:-$XWIN/bin/llvm-lib}"
    export CFLAGS_x86_64_pc_windows_msvc="${CFLAGS_x86_64_pc_windows_msvc:--isystem $XWIN/crt/include -isystem $XWIN/sdk/include/ucrt -isystem $XWIN/sdk/include/shared -isystem $XWIN/sdk/include/um}"
    export RUSTFLAGS="${RUSTFLAGS:-} -C linker=rust-lld \
-C link-arg=/libpath:$XWIN/crt/lib/x86_64 \
-C link-arg=/libpath:$XWIN/sdk/lib/um/x86_64 \
-C link-arg=/libpath:$XWIN/sdk/lib/ucrt/x86_64"
elif [ "$(uname -s 2>/dev/null || echo unknown)" != "Windows_NT" ] && [ -z "${WINDIR:-}" ]; then
    echo "build.sh: no xwin splat at $XWIN and this is not Windows." >&2
    echo "build.sh: see the header of this script for the one-time provisioning step." >&2
    exit 2
fi

exec cargo build --manifest-path "$HERE/Cargo.toml" --release --target "$TARGET" "$@"
