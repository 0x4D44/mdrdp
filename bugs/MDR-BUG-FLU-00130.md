# MDR-BUG-FLU-00130 — AVC444 luma and chroma presentations still alternate on coloured terminal content

- **State:** Fixed
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
- **State history:** Open (2026-09-14T20:41:13Z, raised via `deltic bugs new --land`) -> Fixed (2026-09-14T21:13:36Z, deltic:auto role=fix run=fix-20260914T204351Z-8b2550a0 branch=task/bug-MDR-BUG-FLU-00130-run-fix-20260914T204351Z-8b2550a0 code=1f8674a gate=manual)

## Observation

Arthur reports continuing flicker on bright red terminal text and patterned usage bars in Temper with mdrdp 0.1.245. Metadata traces show ordered luma and chroma updates for the same rectangles; the LC2-only debounce also misses chroma carried by LC0 and permits refinements through unrelated immediate writes. Successor recurrence of closed MDR-BUG-FLU-00123. Implement a bounded luma presentation grace period, release when pending regions receive chroma, and preserve ordered decoding and a hard latency bound.

Evidence fingerprint: `manual:v1:avc444-luma-and-chroma-presentations-still-alte-b649e6ff695aa9a7`


## Fix

Integrated in `1f8674a`, version `0.1.247`: hold pending luma for at most 50 ms
from the first update, releasing early on accepted chroma coverage. Preserve
wire-order decoding and bracket unframed bitmap callbacks. Bound region tracking
and prevent deadline rearming before a successful current-stamp presentation.

Focused validation: 73 surface, 65 graphics and 69 window tests passed; all
vendored suites passed. Regression mutations were restored after observed
failures. Clippy (`--all-targets -D warnings`) and Windows cross-check passed.
Existing Windows warnings and standalone vendored formatting drift remain.

Design and full evidence are in the 2026.09.14 bounded AVC444 luma-wait HLD and
journal. This is Fixed, not independently visually verified: a live affected
terminal session still needs to confirm the reported flicker has improved.

## Notes
