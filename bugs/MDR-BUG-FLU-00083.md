# MDR-BUG-FLU-00083 — ZGFX match lengths can expand one segment without the protocol limit

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/zgfx-decoder
- **Raised:** 2026-08-23T22:14:05Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T221425Z-ff066d26
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00083-run-fix-20260823T221425Z-ff066d26
- **Owner base:** b8e88ffdd6be073c027908d897dca0cdf5c8c1b9
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T22:14:25Z
- **Owner until:** 2026-08-24T00:14:25Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T22:14:05Z, raised via `deltic bugs new` model=gpt-5.6-sol@high)

## Observation

The ZGFX decoder accepts a wire-controlled match length and asks FixedCircularBuffer to write that many bytes without enforcing the RDP 8.0 bulk compression limit of 65,535 uncompressed bytes per segment. A tiny compressed segment can encode a huge length and keep growing the output until the client stalls or exhausts memory. Reject a token before it would make one segment exceed 65,535 bytes, without partially applying that token.

## Fix

<unfixed — raised only>

## Notes
