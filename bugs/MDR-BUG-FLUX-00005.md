# MDR-BUG-FLUX-00005 — Idle session burns 20-90% of a core: full-frame CPU colorspace conversion on every present

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** render
- **Raised:** 2026-08-18T18:02:47Z
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
- **State history:** Open (2026-08-18T18:02:47Z, raised via `deltic bugs new`)

## Observation

Observed by Arthur in top, 2026-08-18 ~18:50: three idle mdrdp v0.1.63 sessions (no interaction, not focused) at 2560x1440 consuming: temper 88% of a core (67% lifetime avg over 1h44m, 30fps idle stream), crucible 30% (11fps), kiln 18% (13fps). A 5s `sample` of the temper process shows ~90% of main-thread time inside softbuffer present: CA::Render::create_image_by_rendering -> CGContextDrawImage -> vImage 16-bit LUT colorspace conversion of the entire 14.7MB buffer per frame (~22ms/frame CPU), plus present_into full-frame copy (~25% of cost). Session/decode threads are healthy (wake::wait_readable parked, decode ~1.3ms). Every server Damaged event requests a redraw regardless of focus/occlusion (src/window.rs user_event SessionEvent::Damaged), and the server streams frames continuously when idle. Expected: an idle unfocused session should cost a few percent of a core at most. Fix directions: (1) colorspace-match the presented buffer to the display so CA stops software-converting; (2) GPU present (IOSurface/CALayer) per the 2026.08.17 native transport HLD direction; (3) gate presents and send Suppress Output PDU (MS-RDPBCGR 2.2.11.3, vendored ironrdp-pdu suppress_output.rs) when the window is occluded/minimised.

## Fix

<unfixed — raised only>

## Notes
