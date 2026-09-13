# MDR-BUG-FLU-00094 — IOSurface backpressure repeats full 5K redraw work on the input event thread

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** window/presentation-latency
- **Raised:** 2026-08-24T09:51:00Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T152407Z-01671e16
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00094-run-verify-20260913T152407Z-01671e16
- **Owner base:** 0ab0ce4617014549fe2e14f2461fff47cb497bea
- **Owner fingerprint:** sha256:1f7fc4c5369083f583b89fd03d67c24507ce1ff452fe9967bb2ba3e2e5796eda
- **Owner since:** 2026-09-13T15:24:07Z
- **Owner until:** 2026-09-13T17:24:07Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T09:51:00Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T09:58:35Z, deltic:auto role=fix run=fix-20260824T095120Z-a12ed09b branch=task/bug-MDR-BUG-FLU-00094-run-fix-20260824T095120Z-a12ed09b code=ab364fc gate=manual)

## Observation

Observation: SessionApp::redraw fills or converts the full staging frame and draws overlays before FrameBuf::present checks whether any IOSurface is writable. When all three surfaces remain compositor-owned, every 2 ms Busy retry repeats the full 5K CPU work on the winit event thread, delaying keyboard and mouse dispatch. Expected: an all-busy pool defers before expensive rendering while present retains the authoritative recheck. Actual: availability is first checked only after the work is complete.

## Fix

<unfixed — raised only>

## Notes
