# MDR-BUG-FLU-00077 — Service ACL hardening strips access from deployed executables

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** Rhydra deploy
- **Raised:** 2026-08-23T21:42:09Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T123100Z-6776825d
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00077-run-verify-20260913T123100Z-6776825d
- **Owner base:** cce006810de1f0243768320d18e97c57b22a66db
- **Owner fingerprint:** sha256:9dd3f8cc0fa2d041bf709afa8145fda55fe30096efd29183d66cb19ab68e75b3
- **Owner since:** 2026-09-13T12:31:00Z
- **Owner until:** 2026-09-13T14:31:00Z
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
