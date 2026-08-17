#!/usr/bin/env bash
# Cross-build the mdrdp indirect display driver and its creator EXE for
# x86_64-pc-windows-msvc FROM macOS. This script is the toolchain-of-record for the
# latency spike: every workaround it needs is commented here rather than patched into
# the sources or, worse, into ~/.xwin (which is a splat of Microsoft's headers and
# must never be edited).
#
# Prerequisites (one-time, see README.md):
#   * xwin splat of the MSVC CRT + Windows SDK 10.0.26100 at ~/.xwin
#     (brew install xwin; xwin --accept-license --temp splat --output ~/.xwin)
#   * WDK 10.0.26100.6584 NuGet (Microsoft.Windows.WDK.x64) unzipped to ~/.xwin/wdk
#   * Apple clang (driver-mode=cl) and rust-lld from the rustup toolchain
#
# Nothing here links against a real Visual Studio: clang in cl mode does the
# compiling and rust-lld -flavor link does the linking.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

XWIN_DIR="${XWIN_DIR:-$HOME/.xwin}"
WDK_DIR="${WDK_DIR:-$XWIN_DIR/wdk}"
RUST_LLD="${RUST_LLD:-$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/bin/rust-lld}"
CLANG="${CLANG:-clang}"

SDK_VER="10.0.26100.0"
IDDCX_VER="1.6"     # header + stub lib version we compile against
UMDF_VER="2.25"     # must match UmdfLibraryVersion in mdrdp-idd.inf

BUILD="$HERE/build"
OBJ="$BUILD/obj"

for p in "$XWIN_DIR/crt/include" "$XWIN_DIR/sdk/include/um" \
         "$WDK_DIR/c/Include/$SDK_VER/um/iddcx/$IDDCX_VER" \
         "$WDK_DIR/c/Include/wdf/umdf/$UMDF_VER"; do
    [ -d "$p" ] || { echo "missing toolchain path: $p" >&2; exit 1; }
done
[ -x "$RUST_LLD" ] || { echo "missing rust-lld: $RUST_LLD" >&2; exit 1; }

rm -rf "$BUILD"
mkdir -p "$OBJ"

# ---------------------------------------------------------------------------
# Include search path
#
# xwin splits the SDK by family (ucrt/shared/um/winrt); wrl.h lives under winrt/,
# not um/. The WDK adds the UMDF (wdf/umdf/<ver>) and IddCx (um/iddcx/<ver>) trees.
# The WDK's own um/ tree comes AFTER the SDK's so the SDK wins any duplicate.
# ---------------------------------------------------------------------------
INCLUDES=(
    -imsvc "$XWIN_DIR/crt/include"
    -imsvc "$XWIN_DIR/sdk/include/ucrt"
    -imsvc "$XWIN_DIR/sdk/include/shared"
    -imsvc "$XWIN_DIR/sdk/include/um"
    -imsvc "$XWIN_DIR/sdk/include/winrt"
    -imsvc "$XWIN_DIR/sdk/include/cppwinrt"
)

DRIVER_INCLUDES=(
    "${INCLUDES[@]}"
    -imsvc "$WDK_DIR/c/Include/wdf/umdf/$UMDF_VER"
    -imsvc "$WDK_DIR/c/Include/$SDK_VER/um/iddcx/$IDDCX_VER"
    -imsvc "$WDK_DIR/c/Include/$SDK_VER/um"
    -imsvc "$WDK_DIR/c/Include/$SDK_VER/shared"
)

# NOTE ON FILENAME CASE: the WDK ships the IddCx header as "IddCx.h" and the sample
# includes it as <iddcx.h>. That only works on a case-insensitive volume, so Driver.h
# spells it with the on-disk casing instead. Several SDK headers do the same to each
# other (IddCx.h itself includes <Opmapi.h> and <Dxgi.h>), which we cannot fix without
# editing ~/.xwin - so a case-SENSITIVE volume will still need a shim include dir.

CL_COMMON=(
    "$CLANG" --driver-mode=cl -target x86_64-pc-windows-msvc
    /c /nologo /EHsc /W3 /O2 /MT /std:c++17
    /DUNICODE /D_UNICODE /D_WIN64 /D_AMD64_ /DAMD64 /DNOMINMAX
    # Two diagnostics that only fire because clang is stricter than MSVC on
    # Microsoft's own headers/sample, not because of anything we wrote:
    #   duplicate-decl-specifier - WDF_DECLARE_CONTEXT_TYPE expands to
    #     "extern __declspec(selectany) extern ..." in wdfobject.h.
    #   unused-private-field     - IndirectMonitorContext::m_Monitor is unused in the
    #     sample too; it is kept so the class stays diffable against upstream.
    -Wno-duplicate-decl-specifier
    -Wno-unused-private-field
)

DRIVER_DEFS=(
    /DUMDF_DRIVER
    /DUMDF_VERSION_MAJOR=2 /DUMDF_VERSION_MINOR=25
    /DIDDCX_VERSION_MAJOR=1 /DIDDCX_VERSION_MINOR=6
    /DIDDCX_MINIMUM_VERSION_REQUIRED=4
    # _WIN32_WINNT/NTDDI must be >= Win10 or the SDK hides the IddCx-era APIs.
    /D_WIN32_WINNT=0x0A00 /DWINVER=0x0A00 /DNTDDI_VERSION=0x0A000010
)

echo "==> compiling driver/Driver.cpp"
"${CL_COMMON[@]}" "${DRIVER_DEFS[@]}" "${DRIVER_INCLUDES[@]}" \
    /Fo"$OBJ/Driver.obj" -- "$HERE/driver/Driver.cpp"

echo "==> compiling creator/main.cpp"
"${CL_COMMON[@]}" /D_CONSOLE /D_WIN32_WINNT=0x0A00 /DWINVER=0x0A00 \
    "${INCLUDES[@]}" \
    /Fo"$OBJ/creator.obj" -- "$HERE/creator/main.cpp"

# ---------------------------------------------------------------------------
# Link
#
# clang cl-mode needs "--" before an absolute source path or it reads the leading "/"
# as the /U flag. rust-lld -flavor link is lld-link. The UMDF driver is an ordinary DLL: the CRT
# supplies _DllMainCRTStartup and Driver.cpp supplies DllMain, so no /ENTRY is needed.
# ---------------------------------------------------------------------------
LIBPATHS=(
    "/libpath:$XWIN_DIR/crt/lib/x86_64"
    "/libpath:$XWIN_DIR/sdk/lib/um/x86_64"
    "/libpath:$XWIN_DIR/sdk/lib/ucrt/x86_64"
)

DRIVER_LIBPATHS=(
    "${LIBPATHS[@]}"
    "/libpath:$WDK_DIR/c/Lib/$SDK_VER/um/x64/iddcx/$IDDCX_VER"
    "/libpath:$WDK_DIR/c/Lib/wdf/umdf/x64/$UMDF_VER"
)

echo "==> linking build/mdrdp_idd.dll"
"$RUST_LLD" -flavor link \
    /nologo /DLL /MACHINE:X64 \
    "/OUT:$BUILD/mdrdp_idd.dll" \
    "${DRIVER_LIBPATHS[@]}" \
    "$OBJ/Driver.obj" \
    WdfDriverStubUm.lib iddcxstub.lib \
    dxgi.lib d3d11.lib avrt.lib ole32.lib \
    ntdll.lib kernel32.lib
# Library set pruned empirically to exactly what resolves:
#   WdfDriverStubUm.lib - UMDF2 stub; supplies FxDriverEntryUm and the export directive.
#   iddcxstub.lib       - IddCx client stubs (IddCx* entry points).
#   ntdll.lib           - the stub calls DbgPrintEx, which lives in ntdll, not in
#                         kernel32/OneCoreUAP. This is the one non-obvious library.
#   dxgi/d3d11/avrt/ole32 - what Driver.cpp itself calls.
# The sample's OneCoreUAP.lib is NOT needed here (nothing resolves only through it), and
# neither are user32/advapi32; adding them changes no import in the output.

echo "==> linking build/mdrdp-idd-create.exe"
"$RUST_LLD" -flavor link \
    /nologo /SUBSYSTEM:CONSOLE /MACHINE:X64 \
    "/OUT:$BUILD/mdrdp-idd-create.exe" \
    "${LIBPATHS[@]}" \
    "$OBJ/creator.obj" \
    swdevice.lib kernel32.lib
# swdevice.lib is the SDK import lib that resolves SwDeviceCreate/SwDeviceClose; it
# forwards to CFGMGR32.dll, which is what shows up in the EXE's import table.

# The INF must sit beside the DLL for Inf2Cat/pnputil on the Windows host, so build/ is
# the whole deployable package.
cp "$HERE/driver/mdrdp-idd.inf" "$BUILD/mdrdp-idd.inf"

echo
echo "==> sanity checks"
file "$BUILD/mdrdp_idd.dll"
file "$BUILD/mdrdp-idd-create.exe"

echo
echo "==> exported symbols in mdrdp_idd.dll (informational; a UMDF driver normally exports nothing)"
python3 "$HERE/pe-exports.py" "$BUILD/mdrdp_idd.dll"

echo
echo "==> done: $BUILD"
