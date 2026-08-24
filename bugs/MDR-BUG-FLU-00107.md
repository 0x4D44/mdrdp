# MDR-BUG-FLU-00107 — Native transport failure leaves a frozen window open

- **State:** Fixed
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
- **State history:** Open (2026-08-24T12:24:24Z, raised via `deltic bugs new`) -> Fixed (2026-08-24T12:36:04Z, deltic:auto role=fix run=fix-20260824T122528Z-6553d1a4 branch=task/bug-MDR-BUG-FLU-00107-run-fix-20260824T122528Z-6553d1a4 code=3deb088 gate=manual)

## Observation

The native video and input worker failure paths set the stop flag and close peer sockets but never close the window waker. EOF or framing failure therefore leaves the application window alive with frozen pixels and auxiliary threads until the user closes it. Close the waker on terminal native transport failure and prove loopback EOF produces the window-close event.

## Fix

<unfixed — raised only>

## Notes
