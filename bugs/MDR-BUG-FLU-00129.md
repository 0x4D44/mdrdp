# MDR-BUG-FLU-00129 — Windowed RDP resolution stays reduced after a monitor returns

- **State:** Closed
- **Priority:** Should
- **Severity:** Medium
- **Area:** window/resolution
- **Raised:** 2026-09-14T06:44:19Z
- **Discovery source:** Human
- **Owner:** -
- **Owner role:** -
- **Owner run:** -
- **Owner host:** -
- **Owner branch:** -
- **Owner fingerprint:** -
- **Owner since:** -
- **Owner until:** -
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-09-14T06:44:19Z, raised via `deltic bugs new`) -> Fixed (2026-09-14T07:10:25Z, deltic:auto role=fix run=fix-20260914T064508Z-4727eab6 branch=task/bug-MDR-BUG-FLU-00129-run-fix-20260914T064508Z-4727eab6 code=b9603adc5684d47ed3a2a1bce1b751bcd753b623 gate=manual) -> Closed (2026-09-14T11:06:16Z, 0x4D44/Codex verify run=verify-20260914T105020Z-1c8b83f8)

## Observation

With Dynamic resolution enabled, switching an external monitor off can cause an RDP
session that was using a large resolution to renegotiate to the laptop's smaller
resolution. When the monitor is turned back on, the RDP session may be on another
macOS desktop/Space and does not renegotiate back to the larger resolution. Expected:
a transient monitor power change must not permanently replace the chosen session or
window resolution; when the original display/window geometry returns, mdrdp should
renegotiate accordingly. Actual: the session can remain at the smaller resolution
after the monitor returns.

## Fix

Integrated as `b9603ad`. The window preserves the user's chosen geometry across
settled monitor changes, restores a windowed session when a smaller display becomes
available again, and re-syncs fullscreen resolution when a hidden window is revealed.
The policy keeps the prior desired geometry and grants a fresh restore attempt when
the monitor transition settles.

## Verification

The independent verifier passed `window_policy::tests::display_follow_cancels_a_candidate_when_the_baseline_returns` and the 20-test `window_policy` family. The lead passed the same focused regression and family.

Both verifiers disabled candidate cancellation at `src/window_policy.rs:109`. The focused regression failed at its `!follow.pending()` assertion because the transient monitor candidate remained queued. Restoring cancellation made the focused test and all 20 policy tests pass again, with exact source restoration verified. The relevant `src/window.rs` path only forwards observations; no separate window test family exists. No live desktop or RDP runtime was available.

## Notes
