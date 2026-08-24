# MDR-BUG-FLU-00056 — Spike viewer input writes can block its window thread indefinitely

- **State:** Open
- **Priority:** Should
- **Severity:** High
- **Area:** rhydra/measurement-viewer
- **Raised:** 2026-08-23T12:09:38Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260824T081954Z-3084288e
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00056-run-fix-20260824T081954Z-3084288e
- **Owner base:** 54c5d1515133c7d6c934446ee3cfdf20f9ec344f
- **Owner fingerprint:** -
- **Owner since:** 2026-08-24T08:19:54Z
- **Owner until:** 2026-08-24T10:19:54Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T12:09:38Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

The latency-spike viewer calls blocking write_all directly from its winit key handler and sets no write timeout. If the peer accepts but stops reading, repeated key events can fill the socket buffer and freeze input and presentation. Move writes off the window thread or enforce a bounded non-blocking handoff.

## Fix

<unfixed — raised only>

## Notes
