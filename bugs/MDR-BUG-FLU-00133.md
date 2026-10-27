# MDR-BUG-FLU-00133 — Dock icons do not return after Dock restarts

- **State:** Fixed
- **Priority:** Should
- **Severity:** Medium
- **Area:** ui
- **Raised:** 2026-10-27T05:36:27Z
- **Discovery source:** Human
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
- **State history:** Open (2026-10-27T05:36:27Z, raised via `deltic bugs new --land`) -> Fixed (2026-10-27T05:48:35Z, deltic:auto role=fix run=fix-20261027T053636Z-2a515e06 branch=task/bug-MDR-BUG-FLU-00133-run-fix-20261027T053636Z-2a515e06 code=18dc8833c69c944f50d648dcce9f8814ef641fc8 gate=manual)

## Observation

Repro: Open an mdrdp session with its Dock tile visible, then restart the macOS Dock while the session remains running. Expected: the session's mdrdp icon returns to the Dock. Actual: the icon does not reappear.

Evidence fingerprint: `manual:v1:dock-icons-do-not-return-after-dock-restarts`


## Fix

### Fix summary (2026-10-27; code=f5723c224694b75dc51d64dce1822a2dc7f56f9a)

`SessionApp` now tracks the running `com.apple.dock` application while a custom Dock label is active. When AppKit reports a different Dock instance, mdrdp reapplies the existing `NSApplication` icon image. The check uses the session event loop's one-second timer and does not run for sessions without a custom icon.

Regression coverage: `dock::tests::dock_relaunch_requests_icon_reapplication` and `dock::tests::dock_start_after_initially_missing_requests_icon_reapplication`. `deltic timeout 120 cargo test dock::tests --lib </dev/null` passed (9 tests). For fails-before, I disabled `ProcessTracker::observe` restart detection; `deltic timeout 120 cargo test dock::tests::dock_relaunch_requests_icon_reapplication --lib </dev/null` failed at `assert!(tracker.observe(Some(2)))`. Restoring detection made the focused tests pass. `cargo fmt --all -- --check`, `./scripts/check-windows.sh`, and offline viewer metadata validation passed.

The Dock was not restarted interactively during this fix; visual confirmation remains for an independent verifier.

## Notes

Provenance correction: `18dc8833c69c944f50d648dcce9f8814ef641fc8` is the integration's version-bump commit; its diff changes only version manifests and lockfiles. The implementation commit is `f5723c224694b75dc51d64dce1822a2dc7f56f9a`, which contains the Dock monitor and icon reapplication changes. The earlier Fixed history entry is retained because ledger history is append-only; the Fix summary names the actual implementation commit.
