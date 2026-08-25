# MDR-BUG-FLU-00111 — Same-version deploy returns from Rhydra quiesce while its images remain in use

- **State:** Fixed
- **Priority:** Should
- **Severity:** Medium
- **Area:** deploy/rhydra
- **Raised:** 2026-08-24T15:13:36Z
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
- **State history:** Open (2026-08-24T15:13:36Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-25T06:29:21Z, deltic:auto role=fix run=fix-20260825T061032Z-b14210bd branch=task/bug-MDR-BUG-FLU-00111-run-fix-20260825T061032Z-b14210bd code=d7dab631c0fc2b963d1819f473c96ba1b6641586 gate=manual)

## Observation

Observed on Quench while deploying integrated mdrdp v0.1.211 over the same C:\mdrdp\v0.5.0 runtime. Deploy ran the current versioned rhydra-agent.exe uninstall action and then immediately failed its first scp with dest open C:\mdrdp\v0.5.0\rhydra-agent.exe: Failure. Read-only inspection after the failure still found the RhydraAgent service plus agent and server processes running from that directory. Running the same versioned uninstall again, then retrying the copy, succeeded. Expected: a successful quiesce action does not return until every owned service and process has stopped and the target images are replaceable; otherwise deploy must fail at quiesce instead of reaching copy.

## Fix

<unfixed — raised only>

## Notes
