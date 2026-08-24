# MDR-BUG-FLU-00107 — Native transport failure leaves a frozen window open

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** native/session-lifecycle
- **Raised:** 2026-08-24T12:24:24Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260824T122528Z-6553d1a4
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00107-run-fix-20260824T122528Z-6553d1a4
- **Owner base:** 5dec86af0e98bf883f118f17e0454b14b1d16bae
- **Owner fingerprint:** -
- **Owner since:** 2026-08-24T12:25:28Z
- **Owner until:** 2026-08-24T14:25:28Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T12:24:24Z, raised via `deltic bugs new`)

## Observation

The native video and input worker failure paths set the stop flag and close peer sockets but never close the window waker. EOF or framing failure therefore leaves the application window alive with frozen pixels and auxiliary threads until the user closes it. Close the waker on terminal native transport failure and prove loopback EOF produces the window-close event.

## Fix

<unfixed — raised only>

## Notes
