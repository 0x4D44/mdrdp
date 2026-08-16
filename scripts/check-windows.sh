#!/bin/sh
# The Windows cfg-drift guard: type-check every target for x86_64-pc-windows-msvc.
#
# ring compiles C, so the check needs Microsoft's CRT/UCRT headers and an
# msvc-style archiver even though nothing is linked. On macOS provision them once:
#
#   rustup target add x86_64-pc-windows-msvc
#   brew install xwin
#   xwin --accept-license splat --output ~/.xwin
#   mkdir -p ~/.xwin/bin
#   ln -sf "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-ar ~/.xwin/bin/llvm-lib
#
# llvm-ar ships with the rustup llvm-tools component and speaks lib.exe syntax
# when invoked under the name llvm-lib — no Visual Studio required.
#
# On a real Windows machine ~/.xwin is absent and the exports are skipped: the
# native toolchain already knows where its headers are.
set -eu
XWIN="${XWIN_DIR:-$HOME/.xwin}"
if [ -d "$XWIN/crt/include" ]; then
    export CC_x86_64_pc_windows_msvc="${CC_x86_64_pc_windows_msvc:-clang}"
    export AR_x86_64_pc_windows_msvc="${AR_x86_64_pc_windows_msvc:-$XWIN/bin/llvm-lib}"
    export CFLAGS_x86_64_pc_windows_msvc="${CFLAGS_x86_64_pc_windows_msvc:--isystem $XWIN/crt/include -isystem $XWIN/sdk/include/ucrt -isystem $XWIN/sdk/include/shared -isystem $XWIN/sdk/include/um}"
fi
exec cargo check --target x86_64-pc-windows-msvc --all-targets "$@"
