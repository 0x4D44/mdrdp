# MDR-BUG-FLU-00050 — Per-message Rhydra stats writes block the sole video sender

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
- **State history:** Open (2026-08-23T12:09:37Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T12:53:08Z, deltic:auto role=fix run=fix-20260823T123736Z-f215bfb8 branch=task/bug-MDR-BUG-FLU-00050-run-fix-20260823T123736Z-f215bfb8 code=0e362ef gate=manual) -> Closed (2026-09-13T10:12:07Z, 0x4D44/Codex verify run=verify-20260913T100245Z-70f2bef2)

## Observation

With --out enabled, win/send.rs writes and flushes the stats file and sends a stats socket message after each video or rect message on the sole sender thread. The next video cannot be delivered until those operations finish, while send_done_us was stamped before them and therefore hides the stall. Decouple or batch non-critical stats persistence without losing bounded shutdown evidence, and measure sender queue pressure before and after.

## Fix

Rhydra now buffers stats rows and flushes the stats file at a 200 ms interval while the sender makes progress, plus once at shutdown. Each bounded sender batch stably sends payloads before telemetry lines, keeping non-critical stats work behind video delivery while timestamps and frame sequence carry causality.

## Notes

- The scheduling/flush regression was observed red with two failed tests, then passed 2/2.
- Full Rhydra suite passed 312 tests and `scripts/check-windows.sh` passed.
- The 200 ms persistence interval is best-effort while the sender runs. A dead client may
  hold the same thread until the separate five-second socket timeout; a hard disk deadline
  would require a separate persistence thread and is outside this fix.

## Verification

Independent verification confirmed fix commit 0e362ef80678656d3604f4c4326cb145ab37f732. The scheduling rules at tools/latency-spike/server/src/send_schedule.rs:47-65 enforce payload-first ordering and the 200 ms boundary; tools/latency-spike/server/src/win/send.rs:420-449 tracks dirty stats and periodic flushing, and :494-534 applies the ordering and shutdown flush.

The lead `send_schedule::tests` run passed 4/4. The independent verifier reproduced 4/4 and covered both ordering and flush-boundary tests. No live Windows socket run or runtime sender measurement was available on this macOS host. The 200 ms persistence interval remains best-effort while the sender is blocked in socket I/O, as recorded in the original Notes.

As a red root mutant, changing `BatchKind::Line => 1` to `BatchKind::Line => 0` in tools/latency-spike/server/src/send_schedule.rs:52 made `payloads_are_delivered_before_stats_lines` fail with the line retained between payloads. The independent verifier separately made the payload class sort after lines and changed the flush boundary from `>=` to `>`; each selected test failed. The source was restored and the focused four-test module passed again.

The six repository gates passed: `cargo build --locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy --all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and `./scripts/check-windows.sh --locked`. The Windows gate exited 0 and emitted only the existing Rhydra `visual_flow` dead-code, missing icon asset, and unused `width`/`height` warnings.
