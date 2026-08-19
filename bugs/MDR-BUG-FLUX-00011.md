# MDR-BUG-FLUX-00011 — rhydra native: a fresh viewer gets no frame until the desktop changes, so connecting to an idle desktop paints nothing

- **State:** Open
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
- **State history:** Open (2026-08-19T11:18:15Z, raised via `deltic bugs new` model=claude-fable-5@high)

## Observation

On the viewer-connect edge win/pipeline.rs capture_loop sets want_keyframe and calls encoder.request_keyframe(), but the very next capture.acquire() returns Acquired::Timeout on a desktop that is not changing. Nothing is ever handed to the encoder, so the requested keyframe never materialises and the newly connected viewer receives no video at all until something on the remote desktop happens to repaint.

Measured on quench (rhydra 0.3.0, IDD source, healthy pipeline confirmed by an 11-frame run immediately before): 5 back-to-back native connects with an idle desktop, session window 1 s, decoded frames 1/1/0/0/4; the same test at 2 s decoded 1/0/0/0/0. With the session typing as soon as it is up, 5/5 runs decoded 2-7 frames inside a 1 s envelope. So the pipeline is healthy and the variable is solely whether the desktop changed.

The fix is a viewer bootstrap: on the connect edge the server must encode the CURRENT desktop content rather than wait for a new one. Two shapes, and the choice is a design call - re-encode the frame pipeline already retains on the GPU for pixel diffing (works for both sources, needs a PixelDiff accessor and synthesised stamps), or let the source re-yield its current surface (natural for the IDD pool, which always holds the latest slot; impossible for DXGI duplication, which has nothing to hand back when the desktop is static). Same family as MDR-BUG-FLUX-00006.

## Fix

<unfixed — raised only>

## Notes
