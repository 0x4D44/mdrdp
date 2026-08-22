# MDR-BUG-FLU-00042 — Malformed CF_UNICODETEXT can make Rhydra read past the clipboard allocation

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/clipboard
- **Raised:** 2026-08-22T19:40:40Z
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
- **State history:** Open (2026-08-22T19:40:40Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

tools/latency-spike/server/src/win/clipboard.rs:297-314 scans a locked CF_UNICODETEXT allocation until it finds a presumed UTF-16 NUL but never checks GlobalSize. A malformed allocation without an in-range terminator causes an out-of-bounds read and can crash the server. Bound the scan to the allocation and reject unterminated data.

## Fix

<unfixed — raised only>

## Notes
