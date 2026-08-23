# MDR-BUG-FLU-00078 — Rhydra service cannot move its worker token into the console session

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** Rhydra service
- **Raised:** 2026-08-23T21:46:28Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T214751Z-79cfb27f
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00078-run-fix-20260823T214751Z-79cfb27f
- **Owner base:** 74bcda98be42615e655c10ccb741f97bb474cbcf
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T21:47:51Z
- **Owner until:** 2026-08-23T23:47:51Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T21:46:28Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

The integrated LocalSystem service starts, but every console-worker launch fails at SetTokenInformation(TokenSessionId) with access denied, so no control, capture, audio, or input listener appears. Rhydra requested a hand-selected duplicated-token access mask; Sunshine’s proven service path requests TOKEN_ALL_ACCESS for the duplicated LocalSystem primary token. Restore that exact access contract and verify the worker launches on Quench.

## Fix

<unfixed — raised only>

## Notes
