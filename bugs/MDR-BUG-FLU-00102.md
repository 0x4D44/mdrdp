# MDR-BUG-FLU-00102 — Outbound RDP writes can monopolize the session thread indefinitely

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/session-latency
- **Raised:** 2026-08-24T11:43:21Z
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
- **State history:** Open (2026-08-24T11:43:21Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-24T12:10:16Z, deltic:auto role=fix run=fix-20260824T114914Z-228eb27a branch=task/bug-MDR-BUG-FLU-00102-run-fix-20260824T114914Z-228eb27a code=1a8f4e0 gate=manual) -> Closed (2026-09-14T08:22:56Z, 0x4D44/Codex verify run=verify-20260914T080730Z-d704ad13)

## Observation

connect::write_framed uses Write::write_all and flush behind a per-syscall socket timeout. A peer that repeatedly accepts a small amount can reset that timeout forever, blocking input, display processing, and shutdown; observe_egfx also writes without any write timeout. Bound each logical outbound pump batch with one absolute deadline and terminate the session after expiry because a partially written frame cannot be retried safely.

## Fix

`src/connect.rs` now writes each logical outbound batch under one absolute deadline, waits for socket progress instead of retrying at a fixed cadence, and drains inbound TLS when a full-duplex peer blocks the write. `src/session.rs` applies that deadline to input, control, clipboard, and response writes; a partial frame ends the session because it cannot be retried safely (`5fb2728`, `4bff7e5`, `2dc13af`, integrated by `1a8f4e0`).

## Verification

Independent verifier ran the focused dribbling-writer regression and the session socket regression; each selected and passed one test. The connect-filtered family passed 24 tests and the session family passed 96 tests. Changing the deadline check at `src/connect.rs:431` to allow writes after the first partial write made the focused test fail at `src/connect.rs:1148`: it wrote 10 bytes instead of the expected 4. Restoration passed the focused and family runs with a clean worktree.

Lead verifier ran `dribbling_writer_honours_one_absolute_deadline`, which passed. The same root-cause mutation failed at `src/connect.rs:1144` because the test received `Ok(())` and `unwrap_err()` panicked. No live RDP session was run; the deterministic transport doubles exercise the bounded-write behavior directly.
