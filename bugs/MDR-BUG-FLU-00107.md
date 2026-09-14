# MDR-BUG-FLU-00107 — Native transport failure leaves a frozen window open

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** native/session-lifecycle
- **Raised:** 2026-08-24T12:24:24Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260914T085023Z-b94c304a
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00107-run-verify-20260914T085023Z-b94c304a
- **Owner base:** ecf5414290b65589548afd6663257564949bfe77
- **Owner fingerprint:** sha256:334f94f15195c94c7d1536df72f2b9b9becad35d49cf88776cbda63a9489f9e1
- **Owner since:** 2026-09-14T08:50:23Z
- **Owner until:** 2026-09-14T10:50:23Z
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
