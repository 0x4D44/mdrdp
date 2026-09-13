# MDR-BUG-FLU-00052 — Spike viewer decode-side stats flushes can delay video processing

- **State:** Fixed
- **Priority:** Could
- **Severity:** Medium
- **Area:** rhydra/measurement-viewer
- **Raised:** 2026-08-23T12:09:37Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T101326Z-2e454244
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00052-run-verify-20260913T101326Z-2e454244
- **Owner base:** 73ec7c8f95ed61b090b2038f4c40e8a7561c463f
- **Owner fingerprint:** sha256:0dbb9dbf138fda48b31e0185a620db248b8eef0730ae3e7ceeb9cc03a476c040
- **Owner since:** 2026-09-13T10:13:26Z
- **Owner until:** 2026-09-13T12:13:26Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T12:09:37Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T09:16:17Z, deltic:auto role=fix run=fix-20260824T084803Z-0a28dbc6 branch=task/bug-MDR-BUG-FLU-00052-run-fix-20260824T084803Z-0a28dbc6 code=67e0326 gate=manual)

## Observation

The latency-spike viewer records several decode-side error, suppression, skip, and displacement rows by writing and flushing its stats file on the same thread that pumps and decodes video. A slow stats destination delays later decode work and distorts the harness. Move or batch those writes so instrumentation does not materially change the path it measures.

## Fix

<unfixed — raised only>

## Notes
