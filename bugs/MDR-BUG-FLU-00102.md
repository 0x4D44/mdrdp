# MDR-BUG-FLU-00102 — Outbound RDP writes can monopolize the session thread indefinitely

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/session-latency
- **Raised:** 2026-08-24T11:43:21Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260824T114914Z-228eb27a
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00102-run-fix-20260824T114914Z-228eb27a
- **Owner base:** c01992f5644c02ca8a1c295ae740c84dc4f89f54
- **Owner fingerprint:** -
- **Owner since:** 2026-08-24T11:49:14Z
- **Owner until:** 2026-08-24T13:49:14Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T11:43:21Z, raised via `deltic bugs new` model=gpt-5.6-sol@high)

## Observation

connect::write_framed uses Write::write_all and flush behind a per-syscall socket timeout. A peer that repeatedly accepts a small amount can reset that timeout forever, blocking input, display processing, and shutdown; observe_egfx also writes without any write timeout. Bound each logical outbound pump batch with one absolute deadline and terminate the session after expiry because a partially written frame cannot be retried safely.

## Fix

<unfixed — raised only>

## Notes
