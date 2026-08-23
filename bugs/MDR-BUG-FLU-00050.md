# MDR-BUG-FLU-00050 — Per-message Rhydra stats writes block the sole video sender

- **State:** Open
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
- **State history:** Open (2026-08-23T12:09:37Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

With --out enabled, win/send.rs writes and flushes the stats file and sends a stats socket message after each video or rect message on the sole sender thread. The next video cannot be delivered until those operations finish, while send_done_us was stamped before them and therefore hides the stall. Decouple or batch non-critical stats persistence without losing bounded shutdown evidence, and measure sender queue pressure before and after.

## Fix

<unfixed — raised only>

## Notes
