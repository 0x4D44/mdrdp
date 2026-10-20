# MDR-BUG-FLU-00120 — IDD layout change reuses the installed driver version

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/idd-deploy
- **Raised:** 2026-08-25T06:47:35Z
- **Discovery source:** Automation
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
- **State history:** Open (2026-08-25T06:47:35Z, raised via `deltic bugs new`) -> Fixed (2026-08-25T06:51:06Z, deltic:auto role=fix run=fix-20260825T064809Z-36a94461 branch=task/bug-MDR-BUG-FLU-00120-run-fix-20260825T064809Z-36a94461 code=809d72f gate=manual) -> Closed (2026-09-14T10:26:51Z, 0x4D44/Codex verify run=verify-20260914T101448Z-8d55f9b8)

## Observation

Deploying the integrated layout-v2 Rhydra stack to Quench leaves the active IDD publishing layout_version 1. The server reports that it speaks layout 2 and exits repeatedly because deploy sees active DriverVer 0.3.0.2 matching the unchanged INF and skips driver replacement. A shared-pool ABI change must advance DriverVer so Windows stages and activates the matching driver package.

## Fix

`809d72f` advances the IDD INF `DriverVer` from `0.3.0.2` to `0.3.0.3` when
the shared-pool ABI moves to layout version 2. The change is integrated in
`809d72f`.

## Verification

The independent verifier passed
`deploy::tests::layout_v2_uses_a_fresh_driver_package_identity` and its
36-test deploy family.

The independent verifier changed
`tools/latency-spike/idd/driver/mdrdp-idd.inf:21` from `0.3.0.3` to `0.3.0.2`.
The focused test failed at `src/deploy.rs:1947`, with `0.3.0.2` on the left
and `0.3.0.3` on the right. The lead made the same mutation and observed the
same failure. Restoring the INF made the focused test and the lead's 40-test
deploy family pass again. No live Windows or RDP runtime was used.

## Notes
