# MDR-BUG-FLU-00053 — Spike viewer copies the full 5K canvas for each small rect update

- **State:** Fixed
- **Priority:** Could
- **Severity:** Medium
- **Area:** rhydra/measurement-viewer
- **Raised:** 2026-08-23T12:09:37Z
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
- **State history:** Open (2026-08-23T12:09:37Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-25T06:56:34Z, deltic:auto role=fix run=fix-20260825T063002Z-25dbfdab branch=task/bug-MDR-BUG-FLU-00053-run-fix-20260825T063002Z-25dbfdab code=533b3e7 gate=manual)

## Observation

The latency-spike viewer publishes an Arc snapshot that the UI retains, then Arc::make_mut clones the full 5120x2880 RGBA canvas before applying the next small rect. That is about 56.25 MiB copied per typing-class update and materially distorts rect latency. Use a bounded mutable surface handoff that preserves snapshot coherence without full-canvas copy-on-write per rect.

## Fix

<unfixed — raised only>

## Notes
