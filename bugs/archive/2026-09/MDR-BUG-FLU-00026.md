# MDR-BUG-FLU-00026 — mdrdp deploy fast path treats same-size stale driver artifacts as an exact stack

- **State:** Closed
- **Priority:** Should
- **Severity:** High
- **Area:** deploy/driver
- **Raised:** 2026-08-20T19:02:37Z
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
- **State history:** Open (2026-08-20T19:02:37Z, raised via `deltic bugs new`) -> Fixed (2026-08-22T00:05:46Z, deltic:auto role=fix run=fix-20260821T235315Z-40c465f7 branch=task/bug-MDR-BUG-FLU-00026-run-fix-20260821T235315Z-40c465f7 code=25e540e gate=manual) -> Closed (2026-09-13T08:28:20Z, 0x4D44/Codex verify run=verify-20260913T081245Z-458a2b4e)

## Observation

Deploying integrated driver 0.3.0.1 over quench's rhydra v0.5.0 returned 'already deployed and healthy — verify only' while the active display driver remained 0.3.0.0 and staged packages did not include 0.3.0.1. src/deploy.rs decides exact_stack from version-directory presence, agent version, and artifact basename sizes before it compares active_driver_ver. The changed DLL and INF kept their prior byte lengths, so the fast path skipped copy and driver installation. Expected: a changed driver package identity can never take the exact-stack fast path, even when file sizes are unchanged.

## Fix

<unfixed — raised only>

## Notes

## Verification

Independent verification confirmed fix commit `25e540e3869075d4d566ed5a6dae63433e2ef174` and the current exact-stack condition in `src/deploy.rs:638-640`, which requires the active driver identity to match before taking the fast path. The focused regression `deploy::tests::stale_active_driver_is_reinstalled_even_while_the_device_is_present` passed, and the restored deploy test suite passed all 36 tests. As a red root mutant, removing `&& driver_current` made the focused regression panic at `src/deploy.rs:2632` with `a stale live driver requires a full deploy`. The identity condition was restored and the deploy suite passed again.

The repository gates then passed: `cargo build --locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy --all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and `./scripts/check-windows.sh --locked`.

No live Quench deployment was attempted because it would require remote access and state-changing driver operations. No new live claim is made. The original stale same-size artifact observation remains the end-to-end product observation.
