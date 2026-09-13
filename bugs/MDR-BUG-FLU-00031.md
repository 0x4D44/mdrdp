# MDR-BUG-FLU-00031 — Native keyboard feedback trails input by seconds

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** native/input
- **Raised:** 2026-08-21T22:15:35Z
- **Discovery source:** Human
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T051543Z-06ee822b
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00031-run-verify-20260913T051543Z-06ee822b
- **Owner base:** 5e3bc3a54dcb6f8cfb2df53c552de14d1b84917c
- **Owner fingerprint:** sha256:a29673da8fbfecaae37de33703a1995c300fdcc60194355059643b85797ccf9d
- **Owner since:** 2026-09-13T05:15:43Z
- **Owner until:** 2026-09-13T07:15:43Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-21T22:15:35Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-21T23:23:59Z, deltic:auto role=fix run=fix-20260821T230833Z-43bbb955 branch=task/bug-MDR-BUG-FLU-00031-run-fix-20260821T230833Z-43bbb955 code=29b607b93e45786b645ffeda727bc545b15e86bb gate=manual)

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

<unfixed — raised only>

## Notes

Measure a release build only. The repository explicitly rejects latency claims
from debug builds.
