# MDR-BUG-FLU-00107 — Native transport failure leaves a frozen window open

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** native/session-lifecycle
- **Raised:** 2026-08-24T12:24:24Z
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
- **State history:** Open (2026-08-24T12:24:24Z, raised via `deltic bugs new`) -> Fixed (2026-08-24T12:36:04Z, deltic:auto role=fix run=fix-20260824T122528Z-6553d1a4 branch=task/bug-MDR-BUG-FLU-00107-run-fix-20260824T122528Z-6553d1a4 code=3deb088 gate=manual) -> Closed (2026-09-14T09:01:13Z, 0x4D44/Codex verify run=verify-20260914T085023Z-b94c304a)

## Observation

The native video and input worker failure paths set the stop flag and close peer sockets but never close the window waker. EOF or framing failure therefore leaves the application window alive with frozen pixels and auxiliary threads until the user closes it. Close the waker on terminal native transport failure and prove loopback EOF produces the window-close event.

## Fix

`6027aa3` makes the video and input workers close the native window waker on
their first terminal transport failure, while preserving intentional shutdown
and suppressing duplicate close notifications. The change is integrated in
`3deb088`.

## Verification

The lead and independent verifiers ran the video EOF, input EOF, and malformed
video framing regressions; all selected tests passed. The native-session family
passed 64 tests for both verifiers.

The independent verifier disabled the `close_window()` calls at
`src/native/session.rs:1177` and `src/native/session.rs:1205`. All three focused
tests then failed with `Err(Empty)` instead of the expected `Ok(())`. The lead
made the same mutation and observed the same missing close signal. Restoring
the calls made the lead EOF and malformed-framing checks pass again, and the
independent verifier reran the focus and 64-test family successfully. The
regressions use loopback sockets; no live RDP runtime was used.

## Notes
