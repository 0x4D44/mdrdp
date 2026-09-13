# MDR-BUG-FLU-00083 — ZGFX match lengths can expand one segment without the protocol limit

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/zgfx-decoder
- **Raised:** 2026-08-23T22:14:05Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T131957Z-a4b023ea
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00083-run-verify-20260913T131957Z-a4b023ea
- **Owner base:** 7523dbd06214161e06f8a61bf76b0cf4a552affc
- **Owner fingerprint:** sha256:02c687bfe6917faf950877fabd9d1041f230f3108c924b65599068cf4f727c6e
- **Owner since:** 2026-09-13T13:19:57Z
- **Owner until:** 2026-09-13T15:19:57Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T22:14:05Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-23T22:23:06Z, deltic:auto role=fix run=fix-20260823T221425Z-ff066d26 branch=task/bug-MDR-BUG-FLU-00083-run-fix-20260823T221425Z-ff066d26 code=1eddbb5 gate=manual)

## Observation

The ZGFX decoder accepts a wire-controlled match length and asks FixedCircularBuffer to write that many bytes without enforcing the RDP 8.0 bulk compression limit of 65,535 uncompressed bytes per segment. A tiny compressed segment can encode a huge length and keep growing the output until the client stalls or exhausts memory. Reject a token before it would make one segment exceed 65,535 bytes, without partially applying that token.

## Fix

<unfixed — raised only>

## Notes
