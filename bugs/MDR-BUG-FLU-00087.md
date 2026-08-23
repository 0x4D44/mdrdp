# MDR-BUG-FLU-00087 — Remote CLIPRDR data request bypasses the to-remote clipboard policy

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/clipboard-policy
- **Raised:** 2026-08-23T22:57:27Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T225750Z-4c02622f
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00087-run-fix-20260823T225750Z-4c02622f
- **Owner base:** d65656a62e64038e778114391a5d355cf458dcaf
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T22:57:50Z
- **Owner until:** 2026-08-24T00:57:50Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T22:57:27Z, raised via `deltic bugs new` model=gpt-5.6-sol@max)

## Observation

ClipboardBridge::handle_local_data_requested reads and returns local clipboard content without checking allow_to_remote. A stale or unsolicited remote FormatDataRequest can therefore retrieve local text or image data even when policy disables sending clipboard content. Reject the request with an error response before any OS clipboard read, while preserving CLIPRDR progress.

## Fix

<unfixed — raised only>

## Notes
