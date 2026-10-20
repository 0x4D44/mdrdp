# MDR-BUG-FLU-00051 — Rhydra 5K queue can drop one tile from an otherwise complete frame

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/server-latency
- **Raised:** 2026-08-23T12:09:37Z
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
- **State history:** Open (2026-08-23T12:09:37Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T13:17:06Z, deltic:auto role=fix run=fix-20260823T125338Z-38f85501 branch=task/bug-MDR-BUG-FLU-00051-run-fix-20260823T125338Z-38f85501 code=1dec4bb gate=manual) -> Closed (2026-09-13T10:25:11Z, 0x4D44/Codex verify run=verify-20260913T101316Z-dc0d6d7c)

## Observation

The server outbound queue holds two messages, while one 5K capture can enqueue a rect update plus two H.264 tile access units. Under sender lag, rect plus tile 0 fill the queue and tile 1 is dropped; the native client cannot complete that logical frame and waits for recovery. Make backpressure and dropping operate on complete logical frames, or otherwise prove both tiles remain coherent without adding stale backlog.

## Fix

The server now assembles every advertised tile for a capture sequence before
admitting one `FrameSet` to the bounded sender queue. Pending assemblies are capped
at three logical frames; expiry and late-tile retirement are explicit. A dropped
set enters a recovery fence which suppresses complete delta sets until every tile
in one sequence is a keyframe, and mixed keyframe responses request alignment again.

Dirty rectangles retain their independent immediate path. This is safe because the
client advances pixel exactness from a complete rect update while continuing to
decode suppressed tile AUs for reference state; bundling rects behind both encoders
would add avoidable interaction latency. Telemetry schema 10 now counts each logical
frame withheld by assembly, recovery, or queue admission once.

Portable tests cover two-tile and one-tile completion, out-of-order callbacks,
duplicates, bounded expiry, reconnect retirement, and recovery after queue pressure.
The full Rhydra test suite and the Windows cross-target check pass.

## Notes

## Verification

Independent verification confirmed fix commit `1dec4bb7366d4eb589e99ee629dbe429f01e815f`. The portable regression suite `logical_frame::tests` ran on the verification build and passed 10/10, covering complete two-tile assembly, capture-order release, bounded whole-plan eviction, late-tile retirement, reconnect retirement, and recovery fencing. These fixtures exercise the original failure mode: a logical frame cannot be admitted or reported complete until its required tiles are present.

As a red root mutant, the `PlannedAssembler` pending-budget condition in `tools/latency-spike/server/src/logical_frame.rs` was changed from `while self.pending.len() > self.max_pending` to `>=`. `logical_frame::tests::planned_frame_budget_drops_the_oldest_whole_plan` then failed with exit 101: `assertion left == right failed`, `left: 1`, `right: 0`. The source was restored with an empty diff, and the 10-test suite passed again. The independent verifier reproduced the same mutant failure in a disposable copy and confirmed the restoration.

The six repository gates passed on this tree: `cargo build --locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy --all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and `./scripts/check-windows.sh --locked`. The Windows gate exited 0 with only the existing icon and unused `width`/`height` warnings.

No live Windows, GPU, network, or Rhydra runtime evidence was collected or claimed.
