# MDR-BUG-FLU-00042 — Malformed CF_UNICODETEXT can make Rhydra read past the clipboard allocation

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/clipboard
- **Raised:** 2026-08-22T19:40:40Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T091158Z-e10300ab
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00042-run-verify-20260913T091158Z-e10300ab
- **Owner base:** 17b713cf3d249120950a9211a7fe3b5f845fd3b0
- **Owner fingerprint:** sha256:b7d1121e8f4d05464af4df07f08241f12a1e30e4d2586027e3186e9554bfc5c1
- **Owner since:** 2026-09-13T09:11:58Z
- **Owner until:** 2026-09-13T11:11:58Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-22T19:40:40Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T12:23:17Z, deltic:auto role=fix run=fix-20260823T121122Z-dad0a66c branch=task/bug-MDR-BUG-FLU-00042-run-fix-20260823T121122Z-dad0a66c code=ca2fa8e gate=manual)

## Observation

tools/latency-spike/server/src/win/clipboard.rs:297-314 scans a locked CF_UNICODETEXT allocation until it finds a presumed UTF-16 NUL but never checks GlobalSize. A malformed allocation without an in-range terminator causes an out-of-bounds read and can crash the server. Bound the scan to the allocation and reject unterminated data.

## Fix

Bound the Win32 handle with `GlobalSize` before constructing a Rust slice, reject zero,
odd-sized, and unterminated allocations, and use an RAII guard so every successful
`GlobalLock` is unlocked on both success and error exits. Portable helper tests cover the
malformed allocation and rounded slack without needing to dereference hostile memory in a
unit test.

## Notes

- Regression observed red before implementation: the unterminated allocation was accepted.
- Focused result after the fix: 29 clipboard tests passed; `scripts/check-windows.sh` passed.
