# MDR-BUG-FLU-00087 — Remote CLIPRDR data request bypasses the to-remote clipboard policy

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/clipboard-policy
- **Raised:** 2026-08-23T22:57:27Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T140308Z-ab5fc370
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00087-run-verify-20260913T140308Z-ab5fc370
- **Owner base:** 6136b50ff78e50a75aa2a6415f43f0b807fdfc7e
- **Owner fingerprint:** sha256:e6cde3c28812ca5ba1ea0f43f7e41ab3ffdae2321b50f45742e922a715e55029
- **Owner since:** 2026-09-13T14:03:08Z
- **Owner until:** 2026-09-13T16:03:08Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T22:57:27Z, raised via `deltic bugs new` model=gpt-5.6-sol@max) -> Fixed (2026-08-23T23:03:17Z, deltic:auto role=fix run=fix-20260823T225750Z-4c02622f branch=task/bug-MDR-BUG-FLU-00087-run-fix-20260823T225750Z-4c02622f code=3d2f34c gate=manual)

## Observation

ClipboardBridge::handle_local_data_requested reads and returns local clipboard content without checking allow_to_remote. A stale or unsolicited remote FormatDataRequest can therefore retrieve local text or image data even when policy disables sending clipboard content. Reject the request with an error response before any OS clipboard read, while preserving CLIPRDR progress.

## Fix

<unfixed — raised only>

## Notes
