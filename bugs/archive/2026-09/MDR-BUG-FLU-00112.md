# MDR-BUG-FLU-00112 — Fresh visible RDP session can show only incremental damage until the host forces a full repaint

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** session/rdp presentation
- **Raised:** 2026-08-24T16:59:47Z
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
- **State history:** Open (2026-08-24T16:59:47Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T17:05:26Z, deltic:auto role=fix run=fix-20260824T170017Z-aa80616e branch=task/bug-MDR-BUG-FLU-00112-run-fix-20260824T170017Z-aa80616e code=91e04d0 gate=manual) -> Closed (2026-09-13T05:52:55Z, independent verifier: the selected first-refresh test passed, its startup-request mutant failed the named assertion, the session and Windows gates passed, model=codex@max)

## Observation

Arthur reported on 2026-08-24 that mdrdp 0.1.211 took a while to show Kiln's remote desktop and then displayed only a flashing command-prompt cursor on black. Switching Windows desktops on Kiln forced the full screen to appear. Live presence showed an ordinary 2560x1440 Avc444v2 RDP session receiving 5.6 MB across 215 painted changes with zero decode errors, so the transport and decoder were alive; the client starts visible without ever sending the full-desktop Refresh Rectangle that its reveal path already requires.

## Fix

The RDP pump now starts with a pending visible-state request. Its first outbound turn
therefore sends Suppress Output's allow form followed by a full-desktop Refresh Rectangle,
using the same ordered path as a later reveal. This closes the startup-only hole: winit
initializes a fresh visible window as not occluded and does not owe the application an
`Occluded(false)` transition, so the old code could wait forever for a visibility event
before asking Windows for complete pixels.

The focused regression `a_fresh_visible_session_requests_a_complete_first_desktop`
passed as one selected test. Replacing only `initial_visibility_request()`'s `Some(true)`
with `None` made that selected test fail its own `session startup must enqueue a
visibility request` assertion. The post-fix session filter passed 114 tests, and
`scripts/check-windows.sh --locked` passed for both app and Rhydra host targets.

The original Kiln observation was reviewed against the startup pump and refresh path,
but this macOS pass did not start a new Windows RDP session.

## Notes
