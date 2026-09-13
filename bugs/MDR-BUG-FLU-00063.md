# MDR-BUG-FLU-00063 — Malformed compressed ZGFX bitstreams panic the RDP client

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/zgfx-decoder
- **Raised:** 2026-08-23T19:39:08Z
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
- **State history:** Open (2026-08-23T19:39:08Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T22:00:30Z, deltic:auto role=fix run=fix-20260823T201502Z-29cd23f9 branch=task/bug-MDR-BUG-FLU-00063-run-fix-20260823T201502Z-29cd23f9 code=2b50693 gate=manual) -> Closed (2026-09-13T11:36:54Z, 0x4D44/Codex verify run=verify-20260913T112913Z-3fd0421b)

## Observation

The compressed ZGFX decoder uses unchecked BitSlice ranges and split_to calls for wire-controlled token fields. A short single-segment payload such as E0 24 00 07 selects a literal token then panics while taking eight absent bits, bypassing the normal protocol-error path. Make bit consumption checked throughout the compressed decoder and regress malformed truncations without unwind.

## Fix

The ZGFX decoder validates padding and bit lengths, checks every wire-controlled bit take, and rejects invalid history offsets so truncated compressed input returns a protocol error instead of panicking.

## Notes

## Verification

The verification build is commit 87a88a06276548a0d3051dee184011bd3c1ee74e and contains the full fix commit 2b50693f8192d5b87712d11eb4ed1822aeee16ae. The fix adds checked Bits consumption, padding validation, and malformed-input coverage in tests/zgfx_malformed.rs:20.

The lead focused command cargo test --locked --test zgfx_malformed truncated_compressed_tokens_return_errors_instead_of_panicking -- --exact passed: 1 passed, 0 failed, 2 filtered out. The regression exercises both a literal token missing its byte and an invalid final padding count. An independent verifier ran the full malformed ZGFX integration file: 3 passed, 0 failed, and also ran the vendored history-offset rejection test: 1 passed.

As a root behavioral mutant, I changed only the checked literal bit take in vendor/ironrdp-graphics/src/zgfx/mod.rs from try_split_to(8) to the unchecked split_to(8). The selected test failed with exit 101 by panicking in bitvec with index 8 out of range: Excluded(1). An independent verifier removed the bounds check inside Bits::try_split_to and reproduced the same panic. Restoring the source made the selected regression pass 1/1 and left an empty source diff.

The six repository gates all exited 0 on this tree: cargo build --locked; cargo test --locked; cargo fmt --all -- --check; cargo clippy --all-targets --locked -- -D warnings; ./scripts/test-vendored.sh; and ./scripts/check-windows.sh --locked. The Windows gate emitted the existing unused width/height warnings in src/present.rs.

No live RDP session, Windows runtime, network input, or fuzz run was exercised, so this closure relies on the offline decoder regressions and source-level mutant evidence.
