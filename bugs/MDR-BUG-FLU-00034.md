# MDR-BUG-FLU-00034 — native doctor ignores saved favourite host and SSH user

- **State:** Open
- **Priority:** Should
- **Severity:** Medium
- **Area:** cli/native diagnostics
- **Raised:** 2026-08-22T00:29:31Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260825T055227Z-f9c740f9
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00034-run-fix-20260825T055227Z-f9c740f9
- **Owner base:** 4767b65c831ec8c309e0575fcd5f3b6d1be8b9d2
- **Owner fingerprint:** -
- **Owner since:** 2026-08-25T05:52:27Z
- **Owner until:** 2026-08-25T07:52:27Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-22T00:29:31Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

With a saved Quench favourite whose host is quench.lan.example and ssh_user is marti, `mdrdp Quench --doctor` diagnoses the literal host Quench as the process user and fails BatchMode key authentication. The normal session path resolves the same favourite correctly. Expected: read-only native diagnostics resolve a favourite name and apply its saved host and SSH identity with the same flag-over-favourite precedence as a session.

## Fix

<unfixed — raised only>

## Notes
