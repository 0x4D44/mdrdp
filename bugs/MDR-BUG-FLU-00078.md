# MDR-BUG-FLU-00078 — Rhydra service cannot move its worker token into the console session

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** Rhydra service
- **Raised:** 2026-08-23T21:46:28Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T125245Z-bb828c49
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00078-run-verify-20260913T125245Z-bb828c49
- **Owner base:** cbc17b69145b29f9c720fb9d77de6571f2a02b90
- **Owner fingerprint:** sha256:479614a76813765d47287f6fcb84c56816da9d41740b455a3df6b75d37dbeccf
- **Owner since:** 2026-09-13T12:52:45Z
- **Owner until:** 2026-09-13T14:52:45Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T21:46:28Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T21:48:01Z, deltic:auto role=fix run=fix-20260823T214751Z-79cfb27f branch=task/bug-MDR-BUG-FLU-00078-run-fix-20260823T214751Z-79cfb27f code=bcfc91f gate=manual)

## Observation

The integrated LocalSystem service starts, but every console-worker launch fails at SetTokenInformation(TokenSessionId) with access denied, so no control, capture, audio, or input listener appears. Rhydra requested a hand-selected duplicated-token access mask; Sunshine’s proven service path requests TOKEN_ALL_ACCESS for the duplicated LocalSystem primary token. Restore that exact access contract and verify the worker launches on Quench.

## Fix

<unfixed — raised only>

## Notes
