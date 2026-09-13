# MDR-BUG-FLU-00087 — Remote CLIPRDR data request bypasses the to-remote clipboard policy

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/clipboard-policy
- **Raised:** 2026-08-23T22:57:27Z
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
- **State history:** Open (2026-08-23T22:57:27Z, raised via `deltic bugs new` model=gpt-5.6-sol@max) -> Fixed (2026-08-23T23:03:17Z, deltic:auto role=fix run=fix-20260823T225750Z-4c02622f branch=task/bug-MDR-BUG-FLU-00087-run-fix-20260823T225750Z-4c02622f code=3d2f34c gate=manual) -> Closed (2026-09-13T14:03:45Z, 0x4D44/Codex verify run=verify-20260913T140308Z-ab5fc370)

## Observation

ClipboardBridge::handle_local_data_requested reads and returns local clipboard content without checking allow_to_remote. A stale or unsolicited remote FormatDataRequest can therefore retrieve local text or image data even when policy disables sending clipboard content. Reject the request with an error response before any OS clipboard read, while preserving CLIPRDR progress.

## Fix

`3d2f34c19ac9248bab628c446ed4e3786c428195` checks `allow_to_remote` before
reading the local OS clipboard or enqueueing the requested data. Disabled requests
receive an explicit CLIPRDR error response while the normal progress path remains
unchanged.

## Notes

## Verification

The verification tree was commit `25e3fc005dc5b7cd2bca4f4c012f1b340934bfdc`,
which contains fix commit `3d2f34c19ac9248bab628c446ed4e3786c428195`.

The lead and independent verifier each ran
`clipboard::tests::to_remote_off_rejects_a_data_request_without_reading_the_clipboard`;
it passed 1/1 in both runs. The independent verifier also ran the 48-test clipboard
family. The full root suite passed 908 library, 17 binary, 5 integration, and 3 ZGFX
tests, with no doctest failures.

For the lead root mutant, I reversed the production guard from
`if !self.allow_to_remote` to `if self.allow_to_remote`. The regression failed at
`src/clipboard.rs:2930`: the fake clipboard read count was 1 instead of 0.
Restoring the fix made the test pass 1/1. The independent verifier reproduced the
same red mutant; source diff and `git diff --check` were clean after restoration.

The six repository gates all exited 0 on the verification tree: `cargo build
--locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy
--all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and
`./scripts/check-windows.sh --locked`. The Windows gate emitted the existing unused
`width`/`height` warnings in `src/present.rs`. The deterministic fake clipboard and
CLIPRDR oracle covered the policy behavior without a live RDP peer.
