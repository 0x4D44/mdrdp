# MDR-BUG-FLU-00077 — Service ACL hardening strips access from deployed executables

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** Rhydra deploy
- **Raised:** 2026-08-23T21:42:09Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T214409Z-8a9b6f88
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00077-run-fix-20260823T214409Z-8a9b6f88
- **Owner base:** f6a31f15c24d64b8b5b46a8e4b0cf5924b26deb1
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T21:44:09Z
- **Owner until:** 2026-08-23T23:44:09Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T21:42:09Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

On the first integrated LocalSystem-service deploy, harden_installation_acl combined recursive /inheritance:r with inheritable root grants. Windows removed inherited ACEs from every descendant after propagation, leaving rhydra-agent.exe with an empty ACL; SCM registered the service but StartService failed with access denied. The install sequence must protect and grant the root first, then explicitly re-enable inheritance below it, with a regression test for command ordering.

## Fix

<unfixed — raised only>

## Notes
