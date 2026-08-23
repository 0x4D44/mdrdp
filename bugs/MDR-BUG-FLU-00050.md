# MDR-BUG-FLU-00050 — Per-message Rhydra stats writes block the sole video sender

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/server-latency
- **Raised:** 2026-08-23T12:09:37Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T123736Z-f215bfb8
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00050-run-fix-20260823T123736Z-f215bfb8
- **Owner base:** 8ad942b2c958d6cc573e0ca2efbda8b141a74233
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T12:37:36Z
- **Owner until:** 2026-08-23T14:37:36Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T12:09:37Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

With --out enabled, win/send.rs writes and flushes the stats file and sends a stats socket message after each video or rect message on the sole sender thread. The next video cannot be delivered until those operations finish, while send_done_us was stamped before them and therefore hides the stall. Decouple or batch non-critical stats persistence without losing bounded shutdown evidence, and measure sender queue pressure before and after.

## Fix

Keep stats in the existing `BufWriter` and flush at a 200 ms interval while the sender is
making progress, plus once at shutdown, instead of flushing after every row. Reorder each
already-bounded sender batch stably as rect payloads, video payloads, then telemetry so
stats cannot split payloads already available to send. Timestamps and `frame_seq`, not
JSONL position, remain the causal contract.

## Notes

- The scheduling/flush regression was observed red with two failed tests, then passed 2/2.
- Full Rhydra suite passed 312 tests and `scripts/check-windows.sh` passed.
- The 200 ms persistence interval is best-effort while the sender runs. A dead client may
  hold the same thread until the separate five-second socket timeout; a hard disk deadline
  would require a separate persistence thread and is outside this fix.
