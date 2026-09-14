# MDR-BUG-FLU-00099 — Partial RDP PDU can monopolize the session thread

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/session-latency
- **Raised:** 2026-08-24T11:29:10Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260914T071757Z-203bddf6
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00099-run-verify-20260914T071757Z-203bddf6
- **Owner base:** 49cf2acedcaf3f593f35498595ba97b833c79489
- **Owner fingerprint:** sha256:c499e7c6a2f369cf78d3391cfdeea347ec1eb5a990f0c278f018da1dfca88fda
- **Owner since:** 2026-09-14T07:17:57Z
- **Owner until:** 2026-09-14T09:17:57Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T11:29:10Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T11:48:59Z, deltic:auto role=fix run=fix-20260824T112930Z-bc42a9c8 branch=task/bug-MDR-BUG-FLU-00099-run-fix-20260824T112930Z-bc42a9c8 code=a2bd5af gate=manual)

## Observation

The session pump calls ironrdp-blocking Framed::read_pdu, which loops until a complete PDU. The socket timeout is per read, so a peer trickling bytes within each 5 ms slice can keep the sole session thread inside one call indefinitely, delaying queued input and preventing shutdown. Reads must return to the pump after currently available bytes while preserving TLS and framed partial state.

## Fix

<unfixed — raised only>

## Notes
