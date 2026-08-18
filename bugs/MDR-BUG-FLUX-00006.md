# MDR-BUG-FLUX-00006 — Reveal after Suppress Output stays black/stale: resume never requests a repaint

- **State:** Fixed
- **Priority:** Should
- **Severity:** High
- **Area:** session
- **Raised:** 2026-08-18T21:54:06Z
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
- **State history:** Open (2026-08-18T21:54:06Z, raised via `deltic bugs new` model=claude-fable-5@high); Fixed (2026-08-18T21:56:00Z, manual land, fix authored same session by claude-fable-5@high)

## Observation

mdrdp connected to kiln showed a permanently black window while the connection stayed healthy (input RTT 30ms, 1 decode error, frames frozen at 80 for the whole session). Root cause: the window connected occluded, Suppress Output was sent, and on reveal the client sent only the AllowDisplayUpdates form of the Suppress Output PDU. Windows under the EGFX pipeline does not repaint on allow - it resumes forwarding only future changes - so everything that changed while suppressed (including the entire first desktop paint after the logon black screen) was never sent, and a static desktop then never repainted. Measured live on kiln 2026-08-18: reveal held 12s, resume PDU sent, zero frames arrived. Fix: send an explicit Refresh Rect PDU (MS-RDPBCGR 2.2.11.2) for the full desktop immediately after the allow (session::visibility_pdus). Validated live on quench: reveal produced a 17-frame repaint burst on an idle desktop.

## Fix

Commit f47e3fa on task/20260818-TSK-HUM-resume-from-suppress-must-request-a-repa:
`session::visibility_pdus` now emits Suppress Output (allow, full inclusive desktop
rect) followed by a Refresh Rect PDU for the same rectangle on reveal; hiding still
sends the bare suppress. Regression test
`revealing_also_requests_a_repaint_of_the_full_desktop` proven red by withholding the
refresh. Validated live against quench (AVC444v2/EGFX): occlude at 9 frames, reveal
produced a 17-frame repaint burst on an idle desktop; the pre-fix binary against kiln
produced 0 frames over a 12 s reveal under the same conditions.

## Notes
