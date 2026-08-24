# MDR-BUG-FLU-00099 — Partial RDP PDU can monopolize the session thread

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/session-latency
- **Raised:** 2026-08-24T11:29:10Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260824T112930Z-bc42a9c8
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00099-run-fix-20260824T112930Z-bc42a9c8
- **Owner base:** 362e7efd2755eb082e197b1266cca9859d59e7e0
- **Owner fingerprint:** -
- **Owner since:** 2026-08-24T11:29:30Z
- **Owner until:** 2026-08-24T13:29:30Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T11:29:10Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

The session pump calls ironrdp-blocking Framed::read_pdu, which loops until a complete PDU. The socket timeout is per read, so a peer trickling bytes within each 5 ms slice can keep the sole session thread inside one call indefinitely, delaying queued input and preventing shutdown. Reads must return to the pump after currently available bytes while preserving TLS and framed partial state.

## Fix

<unfixed — raised only>

## Notes
