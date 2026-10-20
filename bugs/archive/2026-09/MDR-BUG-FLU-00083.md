# MDR-BUG-FLU-00083 — ZGFX match lengths can expand one segment without the protocol limit

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/zgfx-decoder
- **Raised:** 2026-08-23T22:14:05Z
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
- **State history:** Open (2026-08-23T22:14:05Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-23T22:23:06Z, deltic:auto role=fix run=fix-20260823T221425Z-ff066d26 branch=task/bug-MDR-BUG-FLU-00083-run-fix-20260823T221425Z-ff066d26 code=1eddbb5 gate=manual) -> Closed (2026-09-13T13:31:07Z, 0x4D44/Codex verify run=verify-20260913T131957Z-a4b023ea)

## Observation

The ZGFX decoder accepts a wire-controlled match length and asks FixedCircularBuffer to write that many bytes without enforcing the RDP 8.0 bulk compression limit of 65,535 uncompressed bytes per segment. A tiny compressed segment can encode a huge length and keep growing the output until the client stalls or exhausts memory. Reject a token before it would make one segment exceed 65,535 bytes, without partially applying that token.

## Fix

`1eddbb587356ff740e615e5c59d6749ceb935ca4` enforces the 65,535-byte segmented
ZGFX limit before literals, unencoded runs, or match expansions write output. The
encoder also chooses multipart framing when source or compressed data cannot fit
one segment.

## Notes

## Verification

The verification tree was commit `5fccc6f9625cac2658f10c33f7ff0c5b59b8241e`,
which contains fix commit `1eddbb587356ff740e615e5c59d6749ceb935ca4`. The decoder
guard is at `vendor/ironrdp-graphics/src/zgfx/mod.rs:70`; the regression is
`tests/zgfx_malformed.rs:62`.

The lead and independent verifier ran
`compressed_match_cannot_expand_one_segment_past_65535_bytes` (1/1) and the three
malformed-input tests (3/3). The full application suite passed with 908 library,
17 binary, 5 wire-deadline, and 3 ZGFX tests. The vendored sweep passed 47 EGFX,
219 graphics, and 372 PDU tests.

As the lead root mutant, I changed `Some(ZGFX_SEGMENTED_MAXSIZE)` to `None` at
`vendor/ironrdp-graphics/src/zgfx/mod.rs:70`. The exact regression failed at its
own assertion at `tests/zgfx_malformed.rs:76`; restoring the guard made it pass
again. The source diff and working tree were clean after restoration, and the
independent verifier reproduced the same red mutant.

The six repository gates all exited 0 on the verification tree: `cargo build
--locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy
--all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and
`./scripts/check-windows.sh --locked`. The Windows check emitted the existing
unused `width`/`height` warnings in `src/present.rs`.
