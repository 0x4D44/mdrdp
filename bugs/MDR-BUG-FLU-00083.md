# MDR-BUG-FLU-00083 — ZGFX match lengths can expand one segment without the protocol limit

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/zgfx-decoder
- **Raised:** 2026-08-23T22:14:05Z
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
- **State history:** Open (2026-08-23T22:14:05Z, raised via `deltic bugs new` model=gpt-5.6-sol@high)

## Observation

The ZGFX decoder accepts a wire-controlled match length and asks FixedCircularBuffer to write that many bytes without enforcing the RDP 8.0 bulk compression limit of 65,535 uncompressed bytes per segment. A tiny compressed segment can encode a huge length and keep growing the output until the client stalls or exhausts memory. Reject a token before it would make one segment exceed 65,535 bytes, without partially applying that token.

## Fix

<unfixed — raised only>

## Notes
