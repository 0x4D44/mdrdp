# MDR-BUG-FLU-00094 — IOSurface backpressure repeats full 5K redraw work on the input event thread

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** window/presentation-latency
- **Raised:** 2026-08-24T09:51:00Z
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
- **State history:** Open (2026-08-24T09:51:00Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

Observation: SessionApp::redraw fills or converts the full staging frame and draws overlays before FrameBuf::present checks whether any IOSurface is writable. When all three surfaces remain compositor-owned, every 2 ms Busy retry repeats the full 5K CPU work on the winit event thread, delaying keyboard and mouse dispatch. Expected: an all-busy pool defers before expensive rendering while present retains the authoritative recheck. Actual: availability is first checked only after the work is complete.

## Fix

<unfixed — raised only>

## Notes
