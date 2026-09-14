# MDR-BUG-FLU-00096 — RDP reactivation rejects required no-input steps and stalls queued input

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/reactivation
- **Raised:** 2026-08-24T10:49:26Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260914T064719Z-107ae7b7
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00096-run-verify-20260914T064719Z-107ae7b7
- **Owner base:** 14a820b025f7a72d6a3e769c6ecc34e85630e33f
- **Owner fingerprint:** sha256:fc96e113afef47f34bb10b8711bbbef6e334bdbc3bbd093a3e8926f52ab4eb1a
- **Owner since:** 2026-09-14T06:47:19Z
- **Owner until:** 2026-09-14T08:47:19Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T10:49:26Z, raised via `deltic bugs new`) -> Fixed (2026-08-24T11:06:10Z, deltic:auto role=fix run=fix-20260824T104944Z-d7c91cc4 branch=task/bug-MDR-BUG-FLU-00096-run-fix-20260824T104944Z-d7c91cc4 code=0d80453 gate=manual)

## Observation

src/session.rs drive_reactivation treats a None next_pdu_hint as a stalled sequence instead of advancing ConnectionActivationSequence with step_no_input, so a valid reactivation fails at SendSynchronize. While waiting for server PDUs, the same synchronous loop ignores the input doorbell and can strand reliable input for the full 15-second deadline. Advance no-input activation steps in protocol order and service queued input only after Synchronize has reached the wire.

## Fix

<unfixed — raised only>

## Notes
