# MDR-BUG-FLU-00130 — AVC444 luma and chroma presentations still alternate on coloured terminal content

- **State:** Open
- **Priority:** Must
- **Severity:** Medium
- **Area:** graphics/avc444
- **Raised:** 2026-09-14T20:41:13Z
- **Discovery source:** Human
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260914T204351Z-8b2550a0
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00130-run-fix-20260914T204351Z-8b2550a0
- **Owner base:** a3e6514a15eef8c55872cdbc108779ec7434743c
- **Owner fingerprint:** -
- **Owner since:** 2026-09-14T20:43:51Z
- **Owner until:** 2026-09-14T22:43:51Z
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
