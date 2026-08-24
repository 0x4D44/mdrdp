# MDR-BUG-FLU-00096 — RDP reactivation rejects required no-input steps and stalls queued input

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/reactivation
- **Raised:** 2026-08-24T10:49:26Z
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
- **State history:** Open (2026-08-24T10:49:26Z, raised via `deltic bugs new`)

## Observation

src/session.rs drive_reactivation treats a None next_pdu_hint as a stalled sequence instead of advancing ConnectionActivationSequence with step_no_input, so a valid reactivation fails at SendSynchronize. While waiting for server PDUs, the same synchronous loop ignores the input doorbell and can strand reliable input for the full 15-second deadline. Advance no-input activation steps in protocol order and service queued input only after Synchronize has reached the wire.

## Fix

<unfixed — raised only>

## Notes
