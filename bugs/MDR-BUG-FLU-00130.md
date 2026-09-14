# MDR-BUG-FLU-00130 — AVC444 luma and chroma presentations still alternate on coloured terminal content

- **State:** Open
- **Priority:** Must
- **Severity:** Medium
- **Area:** graphics/avc444
- **Raised:** 2026-09-14T20:41:13Z
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
- **State history:** Open (2026-09-14T20:41:13Z, raised via `deltic bugs new --land`)

## Observation

Arthur reports continuing flicker on bright red terminal text and patterned usage bars in Temper with mdrdp 0.1.245. Metadata traces show ordered luma and chroma updates for the same rectangles; the LC2-only debounce also misses chroma carried by LC0 and permits refinements through unrelated immediate writes. Successor recurrence of closed MDR-BUG-FLU-00123. Implement a bounded luma presentation grace period, release when pending regions receive chroma, and preserve ordered decoding and a hard latency bound.

Evidence fingerprint: `manual:v1:avc444-luma-and-chroma-presentations-still-alte-b649e6ff695aa9a7`


## Fix

<unfixed — raised only>

## Notes
