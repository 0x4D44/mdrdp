# MDR-BUG-FLU-00054 — Native video readers can dispatch an unbounded message batch before later video

- **State:** Fixed
- **Priority:** Should
- **Severity:** Medium
- **Area:** rhydra/client-latency
- **Raised:** 2026-08-23T12:09:37Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T102748Z-df5446d7
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00054-run-verify-20260913T102748Z-df5446d7
- **Owner base:** e05b9050034a99a533c37bc0e806d56d3d1440e0
- **Owner fingerprint:** sha256:383c999be1926e3cb320a04a2edbc92d5634caecf6eaadb074fb904e1c848f07
- **Owner since:** 2026-09-13T10:27:48Z
- **Owner until:** 2026-09-13T12:27:48Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T12:09:37Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T20:15:50Z, deltic:auto role=fix run=fix-20260823T200848Z-6f2d92db branch=task/bug-MDR-BUG-FLU-00054-run-fix-20260823T200848Z-6f2d92db code=b67dff4 gate=manual)

## Observation

The production native reader and spike viewer dispatch every complete message from one 64 KiB read before reading or yielding again, and prioritize rect messages. A crafted or pathological batch can run roughly 1600 tiny rect callbacks before a following video callback, delaying decode and paint. Add a bounded dispatch budget or prove a tighter producer-side invariant, while retaining rect priority.

## Fix

<unfixed — raised only>

## Notes
