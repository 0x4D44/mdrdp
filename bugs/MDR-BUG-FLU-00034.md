# MDR-BUG-FLU-00034 — native doctor ignores saved favourite host and SSH user

- **State:** Fixed
- **Priority:** Should
- **Severity:** Medium
- **Area:** cli/native diagnostics
- **Raised:** 2026-08-22T00:29:31Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T083241Z-a495e763
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00034-run-verify-20260913T083241Z-a495e763
- **Owner base:** 9c8bc4784ccbe7152db02e777e084bb8482da418
- **Owner fingerprint:** sha256:b851d993aa061d8694068935cba6bfadab9d914beb7fb622ec3615e445daa2b6
- **Owner since:** 2026-09-13T08:32:41Z
- **Owner until:** 2026-09-13T10:32:41Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-22T00:29:31Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-25T05:58:12Z, deltic:auto role=fix run=fix-20260825T055227Z-f9c740f9 branch=task/bug-MDR-BUG-FLU-00034-run-fix-20260825T055227Z-f9c740f9 code=92b6ffe gate=manual)

## Observation

With a saved Quench favourite whose host is quench.lan.example and ssh_user is marti, `mdrdp Quench --doctor` diagnoses the literal host Quench as the process user and fails BatchMode key authentication. The normal session path resolves the same favourite correctly. Expected: read-only native diagnostics resolve a favourite name and apply its saved host and SSH identity with the same flag-over-favourite precedence as a session.

## Fix

<unfixed — raised only>

## Notes
