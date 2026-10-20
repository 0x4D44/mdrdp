# MDR-BUG-FLU-00117 — Native clipboard calls can block first paint and shutdown indefinitely

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** native/clipboard-lifecycle
- **Raised:** 2026-08-24T18:18:02Z
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
- **State history:** Open (2026-08-24T18:18:02Z, raised via `deltic bugs new` model=gpt-5.6-sol@max) -> Fixed (2026-08-24T18:36:56Z, deltic:auto role=fix run=fix-20260824T181823Z-ef1fd76d branch=task/bug-MDR-BUG-FLU-00117-run-fix-20260824T181823Z-ef1fd76d code=ac72260 gate=manual) -> Closed (2026-09-14T10:07:30Z, 0x4D44/Codex verify run=verify-20260914T094940Z-5547a4d5)

## Observation

Native session startup seeds clipboard state by calling the synchronous OS clipboard before returning the session handle, so a stuck pasteboard read prevents the window and first desktop from starting. Native shutdown also unconditionally joins auxiliary threads that may be stuck in OS clipboard reads or writes, which socket closure cannot interrupt. Move seeding into the poll worker and bound auxiliary joins, detaching only the stuck low-priority clipboard worker.

## Fix

`ac72260` moves the initial clipboard seed into the poll worker, preserves
remote changes until seed completion, and bounds auxiliary teardown so a
worker stuck in an uninterruptible OS clipboard call can be detached. The
change is integrated in `ac72260`.

## Verification

The independent verifier ran four clipboard lifecycle tests and the
native-session family; all passed, including 64 native-session tests.

The independent verifier added a synchronous `read_text()` during auxiliary
startup at `src/native/session.rs:919`. The focused startup test failed at
`src/native/session.rs:3838` with `spawn_aux waited for the OS clipboard seed`.
The lead made the same mutation and observed the failure at
`src/native/session.rs:3837`. The lead also removed the shutdown deadline from
`src/native/session.rs:572`; the bounded-shutdown test failed at
`src/native/session.rs:3993` with `shutdown joined a blocked OS clipboard call`.
Restoring both changes made the 48 clipboard-filtered tests, the auxiliary
teardown test, and the 64-test native-session family pass again. No live RDP
runtime was used.

## Notes
