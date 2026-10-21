# MDR-BUG-FLU-00125 — Scripted duration close can hang past its deadline

- **State:** Fixed
- **Priority:** Should
- **Severity:** High
- **Area:** session/lifecycle
- **Raised:** 2026-08-26T14:03:10Z
- **Discovery source:** Automation
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20261021T043212Z-c94974fd
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00125-run-verify-20261021T043212Z-c94974fd
- **Owner base:** 5f40f6c1524afcd5dd1daea8c8f6c5987c4526c6
- **Owner fingerprint:** sha256:903e5a47ab1558cd9d7900d04fb497b2b37845b372a177a373b558632dcdbe1e
- **Owner since:** 2026-10-21T04:32:12Z
- **Owner until:** 2026-10-21T08:32:12Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-26T14:03:10Z, raised via `deltic bugs new`) -> Fixed (2026-10-20T23:17:37Z, deltic:auto role=fix run=fix-20261020T225938Z-7b868a70 branch=task/bug-MDR-BUG-FLU-00125-run-fix-20261020T225938Z-7b868a70 code=2d0623b gate=manual)

## Observation

A release mdrdp 0.1.237 run against Quench connected successfully, but --duration 8 did not exit before a 60-second outer guard killed the process. No metrics file or graceful-shutdown evidence was produced; inspection found no surviving Quench viewer, and the run was not retried. Investigate the closer-to-window-exit-to-session-join path beginning at src/main.rs scripted duration handling.

## Fix

<unfixed — raised only>

## Notes
