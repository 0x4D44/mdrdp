# MDR-BUG-FLUX-00005 — Idle session burns 20-90% of a core: full-frame CPU colorspace conversion on every present

- **State:** Closed
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
- **State history:** Open (2026-08-18T18:02:47Z, raised via `deltic bugs new`) -> Fixed (2026-08-18T18:51:12Z, deltic:auto role=fix run=fix-20260818T180323Z-p53431-n173223000-c1 branch=task/bug-MDR-BUG-FLUX-00005-run-fix-20260818T180323Z-p53431-n173223000-c1 code=e9575e8 gate=manual) -> Closed (2026-09-13T05:26:31Z, independent verifier: IOSurface presentation and occluded-update suppression passed their focused tests; root mutants failed their named assertions, model=codex@max)

## Observation

Observed by Arthur in top, 2026-08-18 ~18:50: three idle mdrdp v0.1.63 sessions (no interaction, not focused) at 2560x1440 consuming: temper 88% of a core (67% lifetime avg over 1h44m, 30fps idle stream), crucible 30% (11fps), kiln 18% (13fps). A 5s `sample` of the temper process shows ~90% of main-thread time inside softbuffer present: CA::Render::create_image_by_rendering -> CGContextDrawImage -> vImage 16-bit LUT colorspace conversion of the entire 14.7MB buffer per frame (~22ms/frame CPU), plus present_into full-frame copy (~25% of cost). Session/decode threads are healthy (wake::wait_readable parked, decode ~1.3ms). Every server Damaged event requests a redraw regardless of focus/occlusion (src/window.rs user_event SessionEvent::Damaged), and the server streams frames continuously when idle. Expected: an idle unfocused session should cost a few percent of a core at most. Fix directions: (1) colorspace-match the presented buffer to the display so CA stops software-converting; (2) GPU present (IOSurface/CALayer) per the 2026.08.17 native transport HLD direction; (3) gate presents and send Suppress Output PDU (MS-RDPBCGR 2.2.11.3, vendored ironrdp-pdu suppress_output.rs) when the window is occluded/minimised.

## Fix

The integrated fix commits `e9575e81999274e7f849a7b85da8f9a290d7da2a` and
`98c927cf3d8b8571b20a166a0ae32d773468510c` move macOS presentation onto IOSurface and
suppress server updates while the window is occluded. On the post-fix tree,
`cargo test --lib present::tests::` passed 8/8, and the hidden, visible, and reveal
suppression tests in `session::tests` each passed.

The independent red checks reversed the forced-alpha write and the visible desktop
rectangle. The first failed `the_alpha_byte_is_forced_on` with alpha 0 instead of 255;
the second failed `a_visible_window_resumes_updates_with_the_full_inclusive_desktop`
with its `visible must carry the rect` assertion. The original idle 2560x1440 GUI
sample was reviewed verbatim; this macOS pass did not start a new live RDP session.
The source-level presentation and suppression oracles found no residual defect.

## Notes
