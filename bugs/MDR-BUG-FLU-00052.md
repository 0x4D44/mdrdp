# MDR-BUG-FLU-00052 — Spike viewer decode-side stats flushes can delay video processing

- **State:** Open
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
- **State history:** Open (2026-08-23T12:09:37Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

The latency-spike viewer records several decode-side error, suppression, skip, and displacement rows by writing and flushing its stats file on the same thread that pumps and decodes video. A slow stats destination delays later decode work and distorts the harness. Move or batch those writes so instrumentation does not materially change the path it measures.

## Fix

<unfixed — raised only>

## Notes
