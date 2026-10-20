# MDR-BUG-FLU-00059 — A split input burst can strand its tail for 250 ms

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/input-latency
- **Raised:** 2026-08-23T12:28:55Z
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
- **State history:** Open (2026-08-23T12:28:55Z, raised via `deltic bugs new` model=gpt-5.6-sol@max) -> Fixed (2026-08-23T12:33:43Z, deltic:auto role=fix run=fix-20260823T122913Z-6b4401de branch=task/bug-MDR-BUG-FLU-00059-run-fix-20260823T122913Z-6b4401de code=96d3821 gate=manual) -> Closed (2026-09-13T11:16:05Z, 0x4D44/Codex verify run=verify-20260913T110607Z-cb660e0f)

## Observation

The pump drains every queued doorbell before sending one bounded 255-event fast-path batch. If a 256th event was already queued, its doorbell has been consumed and drain_input leaves the event behind; when no server PDU is buffered, the pump can then enter the full 250 ms idle wait. A full batch must force a zero-time readiness check and another pump turn when the socket is not ready, preserving inbound fairness without losing the tail wake.

## Fix

The session pump records when a bounded input drain fills its 255-event fast-path batch. It then uses a zero-duration readiness poll before the next turn, so a queued tail event is serviced promptly while inbound socket processing retains a turn.

## Notes

## Verification

The verification build is commit 102a3a8 and contains the full fix commit 96d38212e247002c809fac0db8740b2b60767dd0. Later session-pump changes renamed the helper to readiness_wait_after_work and retained the same zero-time full-batch behavior.

The focused command cargo test --locked session::tests::an_oversized_input_burst_is_split_across_pump_turns -- --exact passed: 1 passed, 0 failed, 907 filtered out. It queues 256 events, confirms the first drain is a full 255-event batch, requires a zero-duration readiness wait, and proves one tail event remains queued.

As a root behavioral mutant, I changed only Duration::ZERO to IDLE_WAIT in readiness_wait_after_work. The same test failed with exit 101 at src/session.rs:2315: assertion left == right failed, with left 250ms and right 0ns. Restoring the source made the test pass 1/1 and left an empty source diff. An independent verifier reproduced the same 1/1 regression and the same mutant assertion.

The six repository gates all exited 0 on this tree: cargo build --locked; cargo test --locked; cargo fmt --all -- --check; cargo clippy --all-targets --locked -- -D warnings; ./scripts/test-vendored.sh; and ./scripts/check-windows.sh --locked. The Windows gate emitted the existing unused width/height warnings in src/present.rs.

No live RDP server or runtime timing session was exercised, so this closure relies on the deterministic input-pump regression and source-level mutant evidence.
