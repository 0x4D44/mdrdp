# MDR-BUG-FLU-00087 — Remote CLIPRDR data request bypasses the to-remote clipboard policy

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/clipboard-policy
- **Raised:** 2026-08-23T22:57:27Z
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
- **State history:** Open (2026-08-23T22:57:27Z, raised via `deltic bugs new` model=gpt-5.6-sol@max)

## Observation

ClipboardBridge::handle_local_data_requested reads and returns local clipboard content without checking allow_to_remote. A stale or unsolicited remote FormatDataRequest can therefore retrieve local text or image data even when policy disables sending clipboard content. Reject the request with an error response before any OS clipboard read, while preserving CLIPRDR progress.

## Fix

<unfixed — raised only>

## Notes
