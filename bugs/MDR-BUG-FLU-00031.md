# MDR-BUG-FLU-00031 — Native keyboard feedback trails input by seconds

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** native/input
- **Raised:** 2026-08-21T22:15:35Z
- **Discovery source:** Human
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260821T230833Z-43bbb955
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00031-run-fix-20260821T230833Z-43bbb955
- **Owner base:** a9fd77dc37f914f2ec6af27879db4fdbce964427
- **Owner fingerprint:** -
- **Owner since:** 2026-08-21T23:08:33Z
- **Owner until:** 2026-08-22T01:08:33Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-21T22:15:35Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

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
