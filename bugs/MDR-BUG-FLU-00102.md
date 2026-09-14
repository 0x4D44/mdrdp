# MDR-BUG-FLU-00102 — Outbound RDP writes can monopolize the session thread indefinitely

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/session-latency
- **Raised:** 2026-08-24T11:43:21Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260914T080730Z-d704ad13
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00102-run-verify-20260914T080730Z-d704ad13
- **Owner base:** 2f4c734693b6f0427aca0dd8a3d8648496d9e297
- **Owner fingerprint:** sha256:bf1721e90a67cf2b79b5d6b848ef3ddd9710013c5ae3fb6762d2c7a61f6b7371
- **Owner since:** 2026-09-14T08:07:30Z
- **Owner until:** 2026-09-14T10:07:30Z
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
