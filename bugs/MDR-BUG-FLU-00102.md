# MDR-BUG-FLU-00102 — Outbound RDP writes can monopolize the session thread indefinitely

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/session-latency
- **Raised:** 2026-08-24T11:43:21Z
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
- **State history:** Open (2026-08-24T11:43:21Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-24T12:10:16Z, deltic:auto role=fix run=fix-20260824T114914Z-228eb27a branch=task/bug-MDR-BUG-FLU-00102-run-fix-20260824T114914Z-228eb27a code=1a8f4e0 gate=manual)

## Observation

connect::write_framed uses Write::write_all and flush behind a per-syscall socket timeout. A peer that repeatedly accepts a small amount can reset that timeout forever, blocking input, display processing, and shutdown; observe_egfx also writes without any write timeout. Bound each logical outbound pump batch with one absolute deadline and terminate the session after expiry because a partially written frame cannot be retried safely.

## Fix

<unfixed — raised only>

## Notes
