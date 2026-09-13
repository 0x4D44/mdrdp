# MDR-BUG-FLU-00112 — Fresh visible RDP session can show only incremental damage until the host forces a full repaint

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** session/rdp presentation
- **Raised:** 2026-08-24T16:59:47Z
- **Discovery source:** Human
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T054735Z-31b14edb
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00112-run-verify-20260913T054735Z-31b14edb
- **Owner base:** 61e98e48d7dec611bb30c4f860150f7caae1eecb
- **Owner fingerprint:** sha256:c889ce6d976407bf91d06279a66237bfdbe27416a2bd37df6349b30690d011d4
- **Owner since:** 2026-09-13T05:47:35Z
- **Owner until:** 2026-09-13T07:47:35Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T16:59:47Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T17:05:26Z, deltic:auto role=fix run=fix-20260824T170017Z-aa80616e branch=task/bug-MDR-BUG-FLU-00112-run-fix-20260824T170017Z-aa80616e code=91e04d0 gate=manual)

## Observation

Arthur reported on 2026-08-24 that mdrdp 0.1.211 took a while to show Kiln's remote desktop and then displayed only a flashing command-prompt cursor on black. Switching Windows desktops on Kiln forced the full screen to appear. Live presence showed an ordinary 2560x1440 Avc444v2 RDP session receiving 5.6 MB across 215 painted changes with zero decode errors, so the transport and decoder were alive; the client starts visible without ever sending the full-desktop Refresh Rectangle that its reveal path already requires.

## Fix

The RDP pump now starts with a pending visible-state request. Its first outbound turn
therefore sends Suppress Output's allow form followed by a full-desktop Refresh Rectangle,
using the same ordered path as a later reveal. This closes the startup-only hole: winit
initializes a fresh visible window as not occluded and does not owe the application an
`Occluded(false)` transition, so the old code could wait forever for a visibility event
before asking Windows for complete pixels.

The focused regression checks that startup enqueues an enabled visibility request whose
last PDU is the full-desktop refresh. It failed with “session startup must enqueue a
visibility request” before the fix, then passed with all 71 session tests.

## Notes
