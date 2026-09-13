# MDR-BUG-FLU-00091 — EGFX CreateSurface can force a multi-gigabyte client allocation

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/surface-limits
- **Raised:** 2026-08-24T09:25:48Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T144045Z-2107283f
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00091-run-verify-20260913T144045Z-2107283f
- **Owner base:** 2672c6254231f3db1a358f907a1ae6040b030dab
- **Owner fingerprint:** sha256:eea1f3514da900ff4ee338ed8748775bda4edaa83e783ce7b3b3a5dbeb019bd0
- **Owner since:** 2026-09-13T14:40:45Z
- **Owner until:** 2026-09-13T16:40:45Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T09:25:48Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T09:43:53Z, deltic:auto role=fix run=fix-20260824T092607Z-207318fd branch=task/bug-MDR-BUG-FLU-00091-run-fix-20260824T092607Z-207318fd code=d37f299 gate=manual)

## Observation

The EGFX client rejects only zero-sized CreateSurface PDUs, then the mdrdp surface callback allocates width*height*4 bytes directly. A maximum 65535x65535 request asks for about 17.2 GiB and can panic or abort the client before any bitmap arrives. Reject surfaces whose dimensions or total RGBA bytes exceed the negotiated/display safety limit, before invoking the allocation callback, with a no-allocation regression.

## Fix

<unfixed — raised only>

## Notes
