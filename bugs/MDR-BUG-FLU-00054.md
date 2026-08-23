# MDR-BUG-FLU-00054 — Native video readers can dispatch an unbounded message batch before later video

- **State:** Open
- **Priority:** Should
- **Severity:** Medium
- **Area:** rhydra/client-latency
- **Raised:** 2026-08-23T12:09:37Z
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
- **State history:** Open (2026-08-23T12:09:37Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

The production native reader and spike viewer dispatch every complete message from one 64 KiB read before reading or yielding again, and prioritize rect messages. A crafted or pathological batch can run roughly 1600 tiny rect callbacks before a following video callback, delaying decode and paint. Add a bounded dispatch budget or prove a tighter producer-side invariant, while retaining rect priority.

## Fix

<unfixed — raised only>

## Notes
