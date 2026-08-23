# MDR-BUG-FLU-00039 — Silent Rhydra auxiliary peers can hold the only connection forever

- **State:** Open
- **Priority:** Should
- **Severity:** High
- **Area:** rhydra/aux
- **Raised:** 2026-08-22T19:40:40Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T194649Z-c3161b01
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00039-run-fix-20260823T194649Z-c3161b01
- **Owner base:** 50a64a2beba1ff4a2feefd83c42702757215ff01
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T19:46:49Z
- **Owner until:** 2026-08-23T21:46:49Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-22T19:40:40Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

tools/latency-spike/server/src/aux_server.rs:169-176 configures only TCP_NODELAY. The reader and writer at auxchan.rs:399-410 and auxchan.rs:330-340 have no deadlines, so a silent or partial peer can block the serial auxiliary server and a non-reading peer can strand its writer. Add bounded I/O and reconnect behavior.

## Fix

<unfixed — raised only>

## Notes
