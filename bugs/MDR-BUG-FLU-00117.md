# MDR-BUG-FLU-00117 — Native clipboard calls can block first paint and shutdown indefinitely

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** native/clipboard-lifecycle
- **Raised:** 2026-08-24T18:18:02Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260914T094940Z-5547a4d5
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00117-run-verify-20260914T094940Z-5547a4d5
- **Owner base:** 707b54f257c7f617b73d6f47aef96b7e55b316f7
- **Owner fingerprint:** sha256:5f55fb4a1fd757ec8c1c36efe24e8712710672511bdb1e20dc7db574105b6a86
- **Owner since:** 2026-09-14T09:49:40Z
- **Owner until:** 2026-09-14T11:49:40Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T18:18:02Z, raised via `deltic bugs new` model=gpt-5.6-sol@max) -> Fixed (2026-08-24T18:36:56Z, deltic:auto role=fix run=fix-20260824T181823Z-ef1fd76d branch=task/bug-MDR-BUG-FLU-00117-run-fix-20260824T181823Z-ef1fd76d code=ac72260 gate=manual)

## Observation

Native session startup seeds clipboard state by calling the synchronous OS clipboard before returning the session handle, so a stuck pasteboard read prevents the window and first desktop from starting. Native shutdown also unconditionally joins auxiliary threads that may be stuck in OS clipboard reads or writes, which socket closure cannot interrupt. Move seeding into the poll worker and bound auxiliary joins, detaching only the stuck low-priority clipboard worker.

## Fix

<unfixed — raised only>

## Notes
