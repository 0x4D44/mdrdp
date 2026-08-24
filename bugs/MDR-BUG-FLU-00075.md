# MDR-BUG-FLU-00075 — RDP clipboard work can block graphics decode and input on the sole session thread

- **State:** Open
- **Priority:** Should
- **Severity:** High
- **Area:** rdp/clipboard-latency
- **Raised:** 2026-08-23T20:34:25Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260824T065233Z-095a1cce
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00075-run-fix-20260824T065233Z-095a1cce
- **Owner base:** 4b6b439bb70de5f23e31f70a6b91091a389adc32
- **Owner fingerprint:** -
- **Owner since:** 2026-08-24T06:52:33Z
- **Owner until:** 2026-08-24T08:52:33Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T20:34:25Z, raised via `deltic bugs new` model=gpt-5.6-sol@high)

## Observation

The RDP session services an unbounded clipboard action drain before reading graphics, performs OS clipboard and image conversion synchronously, and writes clipboard responses through a blocking socket with no write timeout. A slow pasteboard, large image, or peer that stops reading can delay graphics decode and all input indefinitely. Bound clipboard work per turn and isolate or bound its blocking I/O without breaking static-channel ordering.

## Fix

<unfixed — raised only>

## Notes
