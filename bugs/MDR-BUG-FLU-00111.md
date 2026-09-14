# MDR-BUG-FLU-00111 — Same-version deploy returns from Rhydra quiesce while its images remain in use

- **State:** Closed
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
- **State history:** Open (2026-08-24T15:13:36Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-25T06:29:21Z, deltic:auto role=fix run=fix-20260825T061032Z-b14210bd branch=task/bug-MDR-BUG-FLU-00111-run-fix-20260825T061032Z-b14210bd code=d7dab631c0fc2b963d1819f473c96ba1b6641586 gate=manual) -> Closed (2026-09-14T09:32:30Z, 0x4D44/Codex verify run=verify-20260914T092003Z-a8550dfe)

## Observation

Observed on Quench while deploying integrated mdrdp v0.1.211 over the same C:\mdrdp\v0.5.0 runtime. Deploy ran the current versioned rhydra-agent.exe uninstall action and then immediately failed its first scp with dest open C:\mdrdp\v0.5.0\rhydra-agent.exe: Failure. Read-only inspection after the failure still found the RhydraAgent service plus agent and server processes running from that directory. Running the same versioned uninstall again, then retrying the copy, succeeded. Expected: a successful quiesce action does not return until every owned service and process has stopped and the target images are replaceable; otherwise deploy must fail at quiesce instead of reaching copy.

## Fix

`d7dab63` adds a bounded `verify_quiescence_action` after the versioned
Rhydra uninstall, so deploy proves services, processes, listeners, and target
images are quiescent before copying over a same-version directory.

## Verification

The independent verifier and lead each ran
`deploy::tests::same_version_redeploy_quiesces_before_copy` and
`deploy::tests::quiesce_script_is_bounded_and_checks_every_replaceability_predicate`;
all four focused runs passed. The deploy-test family passed 36 tests for both
verifiers, including the restored lead run.

The independent verifier removed `verify_quiescence_action(target_dir)` from
`src/deploy.rs:502`; the same-version regression failed at the plan assertion.
The lead made the same mutation and observed the assertion fail at
`src/deploy.rs:2452` because the second pre-copy action was no longer the
quiescence verification script. Restoring the action left the focused tests and
36-test deploy family green. No live RDP session or Windows host runtime was
used.

## Notes
