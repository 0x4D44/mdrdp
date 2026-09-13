# MDR-BUG-FLU-00074 — RDP mouse moves use an unbounded stale FIFO ahead of keys and buttons

- **State:** Closed
- **Priority:** Should
- **Severity:** Medium
- **Area:** rdp/input-latency
- **Raised:** 2026-08-23T20:34:25Z
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
- **State history:** Open (2026-08-23T20:34:25Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-23T21:15:15Z, deltic:auto role=fix run=fix-20260823T204628Z-83e09493 branch=task/bug-MDR-BUG-FLU-00074-run-fix-20260823T204628Z-83e09493 code=c1f90b345fe2def611ce48e834d9542621c3c74f gate=manual) -> Closed (2026-09-13T12:28:03Z, 0x4D44/Codex verify run=verify-20260913T121342Z-ac5c720b)

## Observation

Only native sessions use LatestMouseMove. RDP sessions enqueue every pointer move into an unbounded mpsc FIFO, while the session sends at most 255 events per turn. During decode, clipboard work, or slow transport, thousands of obsolete moves can accumulate ahead of a later key or button, adding visible lag and unbounded memory growth. Coalesce lossy motion without reordering reliable input.

## Fix

Physical mouse motion is coalesced in `LatestMouseMove` and drained only after reliable input. The slot is consumed when sent, so stale pointer positions cannot build an unbounded FIFO or replay on a later turn.

## Notes

## Verification

The verification build is commit 3802fdaac1fa79c1a4d048be7f677f46ec4888dc and contains the full fix commit c1f90b345fe2def611ce48e834d9542621c3c74f, landed by 5178f7633a528f56a9ae28c9a6041ff8e0c00ca8. `next_session_input` in `src/session.rs:1391-1402` drains reliable input before the coalesced physical slot, and `LatestMouseMove::take` in `src/input.rs:140-179` consumes that slot after delivery.

The lead focused commands `session::tests::ten_thousand_physical_moves_leave_only_the_final_position_after_a_reliable_key`, `session::tests::scripted_reliable_motion_stays_fifo_before_the_physical_latest_value`, and `session::tests::a_later_drain_does_not_replay_the_physical_latest_value` each passed: 1 passed, 0 failed, 907 filtered out. An independent verifier reran the ten-thousand-move regression and passed 1 test.

As a lead root behavioral mutant, I reversed `next_session_input` so the physical slot was emitted before reliable input. The selected ten-thousand-move regression failed with exit 101 at `src/session.rs:2374`; it observed the mouse event before the reliable key. The independent verifier reproduced the same failure. Restoring the source left `git diff --exit-code -- src/session.rs` empty, and the positive regression passed 1/1 again.

The six repository gates all exited 0 on this tree: `cargo build --locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy --all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and `./scripts/check-windows.sh --locked`. The Windows gate emitted the existing unused width/height warnings in `src/present.rs` and the existing icon-tool warning. Independent hygiene also passed with `git diff --check`.

No live RDP server, GUI compositor, physical display, or Windows runtime was exercised, so this closure relies on the input queue tests and source-level mutant evidence.
