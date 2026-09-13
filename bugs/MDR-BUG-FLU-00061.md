# MDR-BUG-FLU-00061 — EGFX discards and regrows its decompression buffer for every large graphics PDU

- **State:** Fixed
- **Priority:** Must
- **Severity:** Medium
- **Area:** rdp/egfx-latency
- **Raised:** 2026-08-23T12:41:02Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T111913Z-91c668cd
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00061-run-verify-20260913T111913Z-91c668cd
- **Owner base:** 3bf2a1eeec728b27ff8d6316bd7981dd061b0331
- **Owner fingerprint:** sha256:872f81071a6d2877e423dc152f8d46a0b5a3abd8c1b31ff573d58fb541338a00
- **Owner since:** 2026-09-13T11:19:13Z
- **Owner until:** 2026-09-13T13:19:13Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T12:41:02Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T12:43:46Z, deltic:auto role=fix run=fix-20260823T124127Z-14da5e71 branch=task/bug-MDR-BUG-FLU-00061-run-fix-20260823T124127Z-14da5e71 code=dde8c34 gate=manual)

## Observation

GraphicsPipelineClient clears then shrinks its reusable decompression Vec to 16 KiB before every ZGFX payload. Normal large bitmap PDUs exceed that size, so the decode hot path repeatedly discards capacity and allocates it again. Keep clear semantics and the persistent decompressor history, but retain the output buffer capacity across PDUs.

## Fix

<unfixed — raised only>

## Notes
