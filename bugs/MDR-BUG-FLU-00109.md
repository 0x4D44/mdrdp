# MDR-BUG-FLU-00109 — Native input writes have no absolute wall-clock deadline

- **State:** Closed
- **Priority:** Should
- **Severity:** Medium
- **Area:** native/input-latency
- **Raised:** 2026-08-24T12:24:25Z
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
- **State history:** Open (2026-08-24T12:24:25Z, raised via `deltic bugs new`) -> Fixed (2026-08-24T12:55:46Z, deltic:auto role=fix run=fix-20260824T124602Z-f1c450aa branch=task/bug-MDR-BUG-FLU-00109-run-fix-20260824T124602Z-f1c450aa code=e4e2ba5 gate=manual) -> Closed (2026-09-14T09:15:01Z, 0x4D44/Codex verify run=verify-20260914T090527Z-910bbb1e)

## Observation

Native frameless input uses write_all behind a per-syscall timeout. A peer that accepts a small amount before each timeout can keep one input record in flight indefinitely, monopolizing the input worker. Apply one absolute deadline to the logical record and prove a dribbling loopback peer cannot extend it.

## Fix

`44ae7e4` writes each native input record with one absolute wall-clock
deadline, so a peer that accepts bytes slowly cannot extend the record forever.
The change is integrated in `e4e2ba5`.

## Verification

The lead and independent verifiers ran
`native::session::tests::dribbling_native_record_honours_one_absolute_deadline`; each
selected and passed one test. The native-session family passed 64 tests for
both verifiers.

The independent verifier changed the expiry guard at `src/native/session.rs:1624`
to allow writes after a partial write had passed the deadline. The focused test
failed immediately because `unwrap_err()` received `Ok(())`; the mutant did not
hang. The lead made the same mutation and observed the same failure. Restoring
the guard made the focused and 64-test family runs pass again. No live RDP
runtime was used.

## Notes
