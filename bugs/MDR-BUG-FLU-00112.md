# MDR-BUG-FLU-00112 — Fresh visible RDP session can show only incremental damage until the host forces a full repaint

- **State:** Open
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
- **State history:** Open (2026-08-24T16:59:47Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

Arthur reported on 2026-08-24 that mdrdp 0.1.211 took a while to show Kiln's remote desktop and then displayed only a flashing command-prompt cursor on black. Switching Windows desktops on Kiln forced the full screen to appear. Live presence showed an ordinary 2560x1440 Avc444v2 RDP session receiving 5.6 MB across 215 painted changes with zero decode errors, so the transport and decoder were alive; the client starts visible without ever sending the full-desktop Refresh Rectangle that its reveal path already requires.

## Fix

<unfixed — raised only>

## Notes
