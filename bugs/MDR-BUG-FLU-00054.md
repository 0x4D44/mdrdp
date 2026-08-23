# MDR-BUG-FLU-00054 — Native video readers can dispatch an unbounded message batch before later video

- **State:** Open
- **Priority:** Should
- **Severity:** Medium
- **Area:** rhydra/client-latency
- **Raised:** 2026-08-23T12:09:37Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T200848Z-6f2d92db
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00054-run-fix-20260823T200848Z-6f2d92db
- **Owner base:** 9527fac41e6c8167b2df292bff462672b7dbde40
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T20:08:48Z
- **Owner until:** 2026-08-23T22:08:48Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T12:09:37Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

The production native reader and spike viewer dispatch every complete message from one 64 KiB read before reading or yielding again, and prioritize rect messages. A crafted or pathological batch can run roughly 1600 tiny rect callbacks before a following video callback, delaying decode and paint. Add a bounded dispatch budget or prove a tighter producer-side invariant, while retaining rect priority.

## Fix

<unfixed — raised only>

## Notes
