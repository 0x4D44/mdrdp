# MDR-BUG-FLU-00031 — Native keyboard feedback trails input by seconds

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** native/input
- **Raised:** 2026-08-21T22:15:35Z
- **Discovery source:** Human
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
- **State history:** Open (2026-08-21T22:15:35Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-21T23:23:59Z, deltic:auto role=fix run=fix-20260821T230833Z-43bbb955 branch=task/bug-MDR-BUG-FLU-00031-run-fix-20260821T230833Z-43bbb955 code=29b607b93e45786b645ffeda727bc545b15e86bb gate=manual) -> Closed (2026-09-13T05:26:31Z, independent verifier: reliable keyboard input preceded the coalesced final mouse move; the newest-first mutant failed its ordering assertion; model=codex@max)

## Observation

On 2026-08-21 Arthur connected from Flux to Quench with mdrdp v0.1.114 in
native fullscreen mode at 5120x2880. Keyboard feedback appeared several
seconds after typing. A key transition should reach Windows promptly and its
first resulting paint should remain within the interactive latency budget.

The symptom does not yet distinguish input injection latency from delayed
capture, encode, network, decode, or presentation. Rhydra already timestamps
input receive/injection and the native client records input-to-paint, so the
repair must split those stages before changing the input protocol.

## Fix

Commit `29b607b93e45786b645ffeda727bc545b15e86bb` separates physical mouse motion into
a latest-value slot while retaining a reliable FIFO for keyboard, button, wheel, and
scripted input. The post-fix native session suite passed 64/64 and the input suite
passed 49/49, including the 10,000-move and key-order regressions.

The independent red check made `next_native_input` consume the latest mouse move before
the reliable queue. The selected test failed its ordering assertion with the mouse move
before the key. The original several-second keyboard-latency observation was reviewed
against this queue boundary; this pass did not start a new live host session.

## Notes

Measure a release build only. The repository explicitly rejects latency claims
from debug builds.
