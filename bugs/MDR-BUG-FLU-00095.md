# MDR-BUG-FLU-00095 — RDP input latency clock starts after batch encode and socket flush

- **State:** Closed
- **Priority:** Must
- **Severity:** Medium
- **Area:** rdp/input-latency
- **Raised:** 2026-08-24T10:37:08Z
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
- **State history:** Open (2026-08-24T10:37:08Z, raised via `deltic bugs new`) -> Fixed (2026-08-24T10:48:25Z, deltic:auto role=fix run=fix-20260824T103740Z-5d3b7761 branch=task/bug-MDR-BUG-FLU-00095-run-fix-20260824T103740Z-5d3b7761 code=fe7b121 gate=manual) -> Closed (2026-09-14T06:45:25Z, 0x4D44/Codex verify run=verify-20260913T152423Z-ef3d6655)

## Observation

src/session.rs arms input_sent_at only after drain_input has encoded and flushed the complete input batch. Encoding time and socket backpressure are therefore excluded from the displayed input-to-paint latency, understating the delay the user experiences. Start the earliest pending timestamp before encoding, and restore the previous clock if encoding or delivery fails.

## Fix

`src/session.rs:1366-1383` now arms the earliest pending input timestamp before encoding and socket delivery, and restores the previous clock when either step fails (`a84e413`, integrated by `fe7b121`).

## Verification

Independent verifier ran `input_clock_covers_delivery_and_rolls_back_on_failure` plus 96 session tests; all passed. Moving the timestamp arm after delivery made the regression fail at its latency assertion; restoring the original ordering returned the focused and session tests to green with a clean diff.

Lead verifier on tree `8a131b0fc56f16d1cd5dbdca65776dcefde70664` reran the input-clock regression, which passed. Moving `input_sent_at.get_or_insert_with(now)` after `write_framed` made the test fail at `src/session.rs:1827` with `the latency clock must include transport delivery`; restoration passed all five batch-focused tests.

All repository gates passed: `cargo build --locked`, `cargo test --locked`, `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`, `./scripts/test-vendored.sh`, and `./scripts/check-windows.sh --locked`. Windows checking emitted only the existing unused `width` and `height` warnings at `src/present.rs:95`. No live RDP interoperability session was run; the deterministic transport observer regression covers delivery timing and rollback.
