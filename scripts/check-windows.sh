#!/bin/sh
# The Windows cfg-drift guard: type-check every target for x86_64-pc-windows-msvc.
#
# TWO checks, and the second one is the point. mdrdp takes rhydra with
# `default-features = false`, so checking mdrdp alone never compiles rhydra's
# `host` half — which is `src/win/**`, the DXGI duplication, the Media
# Foundation encoder, the SendInput injector and the Win32 clipboard. That is
# to say: checking only mdrdp skipped nearly all of the repo's Windows code.
# Found 2026-08-19, when a Win32 clipboard module with a wrong import passed
# this script and then failed a direct check of the same target.
#
# ring compiles C, so the check needs Microsoft's CRT/UCRT headers and an
# msvc-style archiver even though nothing is linked. On macOS provision them once:
#
#   rustup target add x86_64-pc-windows-msvc
#   brew install xwin
#   xwin --accept-license --temp splat --output ~/.xwin
#   mkdir -p ~/.xwin/bin
#   ln -sf "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-ar ~/.xwin/bin/llvm-lib
#
# --temp is not optional: xwin's --cache-dir defaults to ./.xwin-cache, so
# provisioning from the repo root leaves a 1 GB download cache beside the
# source. Splatting is a one-time step, so nothing wants that cache kept.
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
# 1. mdrdp itself, with rhydra's portable half.
cargo check --target x86_64-pc-windows-msvc --all-targets "$@"

# 2. rhydra with its default `host` feature, which is the only way `src/win/**`
#    is compiled at all.
cd "$(dirname "$0")/../tools/latency-spike/server"
exec cargo check --target x86_64-pc-windows-msvc --all-targets "$@"
