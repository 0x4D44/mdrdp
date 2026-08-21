# MDR-BUG-FLU-00028 — Rhydra agent cannot measure IDD scale above 100 percent because its monitor lookup is DPI-virtualized

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/agent
- **Raised:** 2026-08-21T16:31:35Z
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
- **State history:** Open (2026-08-21T16:31:35Z, raised via `deltic bugs new`) → Fixed (2026-08-21T16:45:51Z, `f856590ab9c8dd6fba98635501dfdab06adf882d`)

## Observation

At 5120x2880 with Windows scale changed from 100% to 200%, Settings visibly applies 200% but rhydra-agent status reports desktop_scale_percent=0 and rejects the exact fullscreen request. The agent mixes physical EnumDisplaySettings geometry with a DPI-virtualized MonitorFromPoint lookup while running DPI-unaware, so the computed point falls outside the logical monitor. Expected: the interactive agent measures the effective 200% scale and allows the exact 5120x2880/200% native connection.

## Fix

Commit `f856590ab9c8dd6fba98635501dfdab06adf882d` sets the rhydra agent's reconcile
thread to per-monitor-v2 DPI awareness before its first display query. This keeps
`EnumDisplaySettingsW`, `MonitorFromPoint`, and `GetScaleFactorForMonitor` in the
same physical coordinate space without changing unrelated threads.

The focused Windows regression
`win::tests::reconcile_thread_selects_per_monitor_v2_dpi_awareness` was compiled
on macOS and executed on Quench as an isolated test binary. Removing the initializer
selected one test and failed its own DPI-context assertion; restoring the initializer
selected one test and passed. Portable server tests passed 299/299, server clippy was
clean with warnings denied, the Windows check passed, and the release server built.

## Notes
