# MDR-BUG-FLU-00076 — Gradual AVC444 luma drift can keep stale chroma detail live indefinitely

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/avc444-chroma
- **Raised:** 2026-08-23T21:33:59Z
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
- **Attempts:** fix=0, doubt=0, indeterminate=1
- **State history:** Open (2026-08-23T21:33:59Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-23T21:41:52Z, deltic:auto role=fix run=fix-20260823T213424Z-4d135b27 branch=task/bug-MDR-BUG-FLU-00076-run-fix-20260823T213424Z-4d135b27 code=0e8cf00aee1caea5f8e22b4e3d222328fc3b14bf gate=manual) -> Closed (2026-09-13T12:50:57Z, 0x4D44/Codex verify run=verify-20260913T123048Z-81a3ee07)

## Observation

The AVC444 stale-chroma guard compares each luma-only block average with only the immediately preceding average. A sequence moving from the last aux-confirmed average in sub-threshold steps (for example 100 to 105 to 110 to 115) never marks the preserved odd-position chroma stale, even after it has drifted beyond the safe threshold. Compare the incoming average directly with the last aux-confirmed average, preserve the return-to-confirmed behavior, and prove the gradual sequence paints the flat current average until auxiliary chroma catches up.

## Fix

The original fix compared each luma average with the last auxiliary-confirmed average, marking gradual drift stale until the auxiliary detail caught up or the average returned within tolerance. A later graphics rewrite retires auxiliary chroma on every touched luma update instead of retaining this average-tracking state.

## Notes

The first verification run `verify-20260913T123048Z-81a3ee07` was indeterminate. The recorded regression `avc444::tests::gradual_luma_drift_marks_chroma_stale_until_return_within_threshold` selected 0 tests on build 36b4504f900806239ad1be009be8d6a60e9e105e because later commit dea775e713bbb52be6d324608f37d292becf3d48 removed the test and the `chroma_stale` / `chroma_confirmed_avg` implementation.

The current successor tests `avc444::tests::a_luma_pass_replaces_previously_delivered_chroma_detail`, `avc444::tests::a_luma_view_wins_even_when_auxiliary_chroma_arrived_first`, `avc444::tests::a_chroma_pass_restores_detail_after_the_main_view`, and the surrounding AVC444 suite passed. A lead mutant replacing `self.clear_chroma(rects)` with `self.clear_chroma(&[])` failed the successor test at `vendor/ironrdp-graphics/src/avc444.rs:824`, then restored to an empty diff. A second independent verifier reran the successor regression and 20-test AVC444 suite, repeated the same root mutant, observed the same assertion failure, restored to an empty diff, and concluded that the later rewrite is a stronger behavioral supersession of the gradual-drift defect.

The six repository gates all exited 0 on this tree: `cargo build --locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy --all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and `./scripts/check-windows.sh --locked`. No live AVC/RDP connection was used. Closure rests on the current invariant that every luma update clears touched auxiliary-chroma validity before writing the luma averages, so no number of gradual luma steps can retain stale auxiliary detail. Fresh chroma can restore detail afterward.
