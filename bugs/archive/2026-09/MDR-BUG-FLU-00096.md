# MDR-BUG-FLU-00096 — RDP reactivation rejects required no-input steps and stalls queued input

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/reactivation
- **Raised:** 2026-08-24T10:49:26Z
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
- **State history:** Open (2026-08-24T10:49:26Z, raised via `deltic bugs new`) -> Fixed (2026-08-24T11:06:10Z, deltic:auto role=fix run=fix-20260824T104944Z-d7c91cc4 branch=task/bug-MDR-BUG-FLU-00096-run-fix-20260824T104944Z-d7c91cc4 code=0d80453 gate=manual) -> Closed (2026-09-14T07:16:11Z, 0x4D44/Codex verify run=verify-20260914T064719Z-107ae7b7)

## Observation

src/session.rs drive_reactivation treats a None next_pdu_hint as a stalled sequence instead of advancing ConnectionActivationSequence with step_no_input, so a valid reactivation fails at SendSynchronize. While waiting for server PDUs, the same synchronous loop ignores the input doorbell and can strand reliable input for the full 15-second deadline. Advance no-input activation steps in protocol order and service queued input only after Synchronize has reached the wire.

## Fix

`src/session.rs` now advances activation states that require no incoming PDU, polls the input doorbell during hinted waits, and drains queued input only after Synchronize reaches the transport (`5e2ec49`, integrated by `0d80453`).

## Verification

Independent verifier ran the three reactivation regressions and the 96-test session family; all passed. Bypassing `step_no_input` produced an invalid zero-length activation output at `src/session.rs:1018`, and restoration returned the focused tests to green with a clean worktree.

Lead verifier ran the three reactivation regressions plus `delayed_hinted_wait_starts_a_fresh_input_write_budget`; all passed before mutation. Moving the input drain before the Synchronize write made `synchronize_is_written_before_reactivation_input_is_drained` fail at `src/session.rs:2097` because the wire began with `4` instead of `0xA5`; restoration reran three reactivation tests and the ordering regression, all green.

All repository gates passed on tree `5efa6e7246357e7a6057845a31b8219eb3f40668`: `cargo build --locked`, `cargo test --locked`, `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`, `./scripts/test-vendored.sh`, and `./scripts/check-windows.sh --locked`. Windows checking emitted only the existing unused `width` and `height` warnings at `src/present.rs:95`. No live RDP interoperability session was run; deterministic reactivation and wire-order tests exercise the recorded protocol behavior.
