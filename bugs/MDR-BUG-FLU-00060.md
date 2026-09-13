# MDR-BUG-FLU-00060 — Malformed ZGFX multipart segment length panics the RDP client

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/zgfx-decoder
- **Raised:** 2026-08-23T12:35:14Z
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
- **State history:** Open (2026-08-23T12:35:14Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T12:39:47Z, deltic:auto role=fix run=fix-20260823T123541Z-8d2b0ce0 branch=task/bug-MDR-BUG-FLU-00060-run-fix-20260823T123541Z-8d2b0ce0 code=0c8f57f gate=manual) -> Closed (2026-09-13T11:16:05Z, 0x4D44/Codex verify run=verify-20260913T110621Z-3ac98203)

## Observation

A server-controlled multipart ZGFX PDU declares each segment length as u32. SegmentedDataPdu::from_buffer passes that length directly to slice::split_at, so a length larger than the remaining payload panics instead of returning the decoder's Result error. Reject a declared segment that exceeds the remaining bytes and add a malformed-PDU regression.

## Fix

The multipart ZGFX decoder uses checked splitting for each declared segment body and maps an overlarge length to UnexpectedEof through ZgfxError. The public decompressor therefore returns an error without panicking or appending output.

## Notes

## Verification

The verification build is commit 102a3a8 and contains the full fix commit 0c8f57f829d12f98833e6312920181a88c5fcd3f. The guard remains in vendor/ironrdp-graphics/src/zgfx/control_messages.rs:34, and the malformed public-path regression is tests/zgfx_malformed.rs:4.

The focused command cargo test --locked --test zgfx_malformed multipart_segment_length_must_fit_remaining_payload -- --exact passed: 1 passed, 0 failed, 2 filtered out. The malformed wire payload declares two bytes while only one remains and leaves the output empty after the decoder returns Err.

As a root behavioral mutant, I replaced the checked guard with the pre-fix buffer.split_at(size). The same test failed with exit 101 by panicking at vendor/ironrdp-graphics/src/zgfx/control_messages.rs:34:61 with mid > len. Restoring the source made the regression pass 1/1 and left an empty source diff. An independent verifier reproduced the same panic mutant and confirmed the restored regression.

The six repository gates all exited 0 on this tree: cargo build --locked; cargo test --locked; cargo fmt --all -- --check; cargo clippy --all-targets --locked -- -D warnings; ./scripts/test-vendored.sh; and ./scripts/check-windows.sh --locked. The Windows gate emitted the existing unused width/height warnings in src/present.rs.

No live RDP server or runtime decoder session was exercised, so this closure relies on the deterministic public decoder regression and source-level mutant evidence.
