# MDR-BUG-FLU-00042 — Malformed CF_UNICODETEXT can make Rhydra read past the clipboard allocation

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/clipboard
- **Raised:** 2026-08-22T19:40:40Z
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
- **State history:** Open (2026-08-22T19:40:40Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T12:23:17Z, deltic:auto role=fix run=fix-20260823T121122Z-dad0a66c branch=task/bug-MDR-BUG-FLU-00042-run-fix-20260823T121122Z-dad0a66c code=ca2fa8e gate=manual) -> Closed (2026-09-13T09:22:49Z, 0x4D44/Codex verify run=verify-20260913T091158Z-e10300ab)

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

## Verification

Independent verification confirmed fix commit ca2fa8e7fc9d1d8ef22af6f1401fcb6c0395fd23. The current Windows reader in tools/latency-spike/server/src/win/clipboard.rs:299-323 obtains `GlobalSize`, creates only a bounded UTF-16 slice, decodes through the shared helper, and unlocks every successful lock with `GlobalLockGuard`. The portable checks are in tools/latency-spike/server/src/clipboard.rs:83-105.

The focused `clipboard::tests` run passed 29/29. It covered rejection of unterminated, zero-byte, and odd-byte allocations, plus first-NUL decoding with rounded slack. The independent verifier also confirmed that the 00048 and 00050 references to `scripts/check-windows.sh` are shared-gate mentions; those fixes do not modify the script or overlap this clipboard path.

As a red root mutant, changing the NUL predicate at tools/latency-spike/server/src/clipboard.rs:102 from `unit == 0` to `unit != 0` made two focused tests fail: `an_unterminated_cf_unicode_text_allocation_is_rejected` and `utf16_text_stops_at_the_first_nul_and_ignores_rounded_slack`. The observed failures were the malformed allocation being accepted and `left: ""`, `right: "A"`. The source was restored and the full 29-test clipboard module passed again. The independent verifier separately changed the missing-NUL error to accept `units.len()` and reproduced the unterminated test failure.

The six repository gates passed: `cargo build --locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy --all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and `./scripts/check-windows.sh --locked`. The Windows check exited 0 and emitted only the existing unused `width`/`height` warnings in src/present.rs. No live Windows clipboard allocation was exercised on this macOS host, so runtime `GlobalSize`/`GlobalLock` coverage remains unverified.
