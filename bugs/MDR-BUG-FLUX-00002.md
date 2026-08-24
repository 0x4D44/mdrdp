# MDR-BUG-FLUX-00002 — cargo check --target x86_64-pc-windows-msvc fails on macOS: libz-sys build script cannot cross-compile C

- **State:** Fixed
- **Priority:** Should
- **Severity:** Medium
- **Area:** build
- **Raised:** 2026-08-16T20:34:32Z
- **Discovery source:** Agent
- **Owner:** -
- **Owner role:** -
- **Owner run:** -
- **Owner host:** -
- **Owner branch:** -
- **Owner base:** -
- **Owner fingerprint:** -
- **Owner since:** -
- **Owner until:** -
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-16T20:34:32Z, raised via `deltic bugs new` model=claude-fable-5) -> Fixed (2026-08-24T23:58:11Z, deltic:auto role=fix run=fix-20260824T235727Z-1e06e976 branch=task/bug-MDR-BUG-FLUX-00002-run-fix-20260824T235727Z-1e06e976 code=dcd80e06249f2fd22776084048c83837aebdac86 gate=manual)

## Observation

The documented Windows drift gate (cargo check --target x86_64-pc-windows-msvc --all-targets, CLAUDE.md 'Keep the Windows target compiling') now fails on macOS before any Rust is type-checked: libz-sys v1.1.29's build script invokes cc with --target=x86_64-pc-windows-msvc and dies compiling zlib (zconf.h: fatal error: 'sys/types.h' file not found) because no MSVC headers exist on the Mac. libz-sys is pulled onto the Windows target via eframe/egui-winit -> arboard -> image -> png -> flate2 and via sspi -> winscard -> flate2, so the break most plausibly arrived with the egui UI stack; the check was recorded clean on 2026-08-14. Until fixed, the repo's only cfg-drift gate for Windows cannot run, so drift lands silently. Likely fix directions for the drain: keep flate2 on its pure-Rust backend for the windows target (find which crate enables the zlib feature), or vendor/point vcpkg headers, or drop the C-backend edge.

## Fix

<unfixed — raised only>

## Notes
