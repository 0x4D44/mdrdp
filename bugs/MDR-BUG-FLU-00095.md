# MDR-BUG-FLU-00095 — RDP input latency clock starts after batch encode and socket flush

- **State:** Fixed
- **Priority:** Must
- **Severity:** Medium
- **Area:** rdp/input-latency
- **Raised:** 2026-08-24T10:37:08Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T152423Z-ef3d6655
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00095-run-verify-20260913T152423Z-ef3d6655
- **Owner base:** ca2a02a8086be85956ef17bff63127e539240e55
- **Owner fingerprint:** sha256:2b29d30f111a5d4683bf462f5d41858694fffc73b5e07e7cd0dd4b17a51365f3
- **Owner since:** 2026-09-13T15:24:23Z
- **Owner until:** 2026-09-13T17:24:23Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T10:37:08Z, raised via `deltic bugs new`) -> Fixed (2026-08-24T10:48:25Z, deltic:auto role=fix run=fix-20260824T103740Z-5d3b7761 branch=task/bug-MDR-BUG-FLU-00095-run-fix-20260824T103740Z-5d3b7761 code=fe7b121 gate=manual)

## Observation

src/session.rs arms input_sent_at only after drain_input has encoded and flushed the complete input batch. Encoding time and socket backpressure are therefore excluded from the displayed input-to-paint latency, understating the delay the user experiences. Start the earliest pending timestamp before encoding, and restore the previous clock if encoding or delivery fails.

## Fix

<unfixed — raised only>

## Notes
