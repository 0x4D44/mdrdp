# MDR-BUG-FLU-00053 — Spike viewer copies the full 5K canvas for each small rect update

- **State:** Open
- **Priority:** Could
- **Severity:** Medium
- **Area:** rhydra/measurement-viewer
- **Raised:** 2026-08-23T12:09:37Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260825T063002Z-25dbfdab
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00053-run-fix-20260825T063002Z-25dbfdab
- **Owner base:** bee6e2f0972d5b40ea99656b82537c3d4359645b
- **Owner fingerprint:** -
- **Owner since:** 2026-08-25T06:30:02Z
- **Owner until:** 2026-08-25T08:30:02Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T12:09:37Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

The latency-spike viewer publishes an Arc snapshot that the UI retains, then Arc::make_mut clones the full 5120x2880 RGBA canvas before applying the next small rect. That is about 56.25 MiB copied per typing-class update and materially distorts rect latency. Use a bounded mutable surface handoff that preserves snapshot coherence without full-canvas copy-on-write per rect.

## Fix

<unfixed — raised only>

## Notes
