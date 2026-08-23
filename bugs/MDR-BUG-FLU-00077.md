# MDR-BUG-FLU-00077 — Service ACL hardening strips access from deployed executables

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** Rhydra deploy
- **Raised:** 2026-08-23T21:42:09Z
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
- **State history:** Open (2026-08-23T21:42:09Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T21:44:21Z, deltic:auto role=fix run=fix-20260823T214409Z-8a9b6f88 branch=task/bug-MDR-BUG-FLU-00077-run-fix-20260823T214409Z-8a9b6f88 code=b8c04fc gate=manual)

## Observation

On the first integrated LocalSystem-service deploy, harden_installation_acl combined recursive /inheritance:r with inheritable root grants. Windows removed inherited ACEs from every descendant after propagation, leaving rhydra-agent.exe with an empty ACL; SCM registered the service but StartService failed with access denied. The install sequence must protect and grant the root first, then explicitly re-enable inheritance below it, with a regression test for command ordering.

## Fix

<unfixed — raised only>

## Notes
