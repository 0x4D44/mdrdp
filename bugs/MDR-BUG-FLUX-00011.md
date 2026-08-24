# MDR-BUG-FLUX-00011 — rhydra native: a fresh viewer gets no frame until the desktop changes, so connecting to an idle desktop paints nothing

- **State:** Fixed
- **Priority:** Should
- **Severity:** High
- **Area:** native-transport
- **Raised:** 2026-08-19T11:18:15Z
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
- **State history:** Open (2026-08-19T11:18:15Z, raised via `deltic bugs new` model=claude-fable-5@high) -> Fixed (2026-08-23T19:42:03Z, deltic:auto role=fix run=fix-20260823T193534Z-bc7d638d branch=task/bug-MDR-BUG-FLUX-00011-run-fix-20260823T193534Z-bc7d638d code=787740f gate=manual) -> Open (2026-08-24T16:50:34Z, reopened after human report reproduced the original static first-frame symptom on mdrdp 0.1.211 against Kiln) -> Fixed (2026-08-24T16:53:19Z, provisional recurrence attribution withdrawn after live presence proved Kiln used RDP/Avc444v2 rather than the native transport; original fix remains code=787740f)

## Observation

On the viewer-connect edge win/pipeline.rs capture_loop sets want_keyframe and calls encoder.request_keyframe(), but the very next capture.acquire() returns Acquired::Timeout on a desktop that is not changing. Nothing is ever handed to the encoder, so the requested keyframe never materialises and the newly connected viewer receives no video at all until something on the remote desktop happens to repaint.

Measured on quench (rhydra 0.3.0, IDD source, healthy pipeline confirmed by an 11-frame run immediately before): 5 back-to-back native connects with an idle desktop, session window 1 s, decoded frames 1/1/0/0/4; the same test at 2 s decoded 1/0/0/0/0. With the session typing as soon as it is up, 5/5 runs decoded 2-7 frames inside a 1 s envelope. So the pipeline is healthy and the variable is solely whether the desktop changed.

The fix is a viewer bootstrap: on the connect edge the server must encode the CURRENT desktop content rather than wait for a new one. Two shapes, and the choice is a design call - re-encode the frame pipeline already retains on the GPU for pixel diffing (works for both sources, needs a PixelDiff accessor and synthesised stamps), or let the source re-yield its current surface (natural for the IDD pool, which always holds the latest slot; impossible for DXGI duplication, which has nothing to hand back when the desktop is static). Same family as MDR-BUG-FLUX-00006.

## Fix

The capture loop now keeps the last admitted desktop texture on the GPU even when
pixel diffing is disabled. On a viewer connect edge it polls the real source first;
if that bounded poll is idle, it submits the retained texture with fresh synthetic
timestamps after requesting an all-tile keyframe. A real frame therefore wins, a
source rebuild invalidates stale pixels before reuse, and a static desktop paints
without requiring user input.

The bootstrap remains armed until a frame passes whole-frame surface admission.
Later recovery keyframe requests may also reuse the retained texture, so a reconnect
cannot freeze merely because its first recovery frame met a full outbound queue.
Portable state tests cover cold missing state, reconnect, rebuild invalidation,
successful admission, and recovery retry.

## Notes

The superficially similar 0.1.211 Kiln report was provisionally attributed here, then
live session presence identified its transport as RDP with AVC444v2. It does not
reproduce or refute this native-transport defect; the RDP repaint path is tracked
separately.
