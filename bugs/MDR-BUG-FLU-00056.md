# MDR-BUG-FLU-00056 — Spike viewer input writes can block its window thread indefinitely

- **State:** Fixed
- **Priority:** Should
- **Severity:** High
- **Area:** rhydra/measurement-viewer
- **Raised:** 2026-08-23T12:09:38Z
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
- **State history:** Open (2026-08-23T12:09:38Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T08:32:06Z, deltic:auto role=fix run=fix-20260824T081954Z-3084288e branch=task/bug-MDR-BUG-FLU-00056-run-fix-20260824T081954Z-3084288e code=a7c63298e0afb3c771009092b7e9bbed6bcb080f gate=manual)

## Observation

The latency-spike viewer calls blocking write_all directly from its winit key handler and sets no write timeout. If the peer accepts but stops reading, repeated key events can fill the socket buffer and freeze input and presentation. Move writes off the window thread or enforce a bounded non-blocking handoff.

## Fix

<unfixed — raised only>

## Notes
