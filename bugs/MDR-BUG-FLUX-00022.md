# MDR-BUG-FLUX-00022 — rhydra HEVC rejects quench's valid Main-tier stream because the contract requires unsupported High tier

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** native-transport
- **Raised:** 2026-08-20T10:57:47Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260820T105810Z-p67382-n663796000-c1
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLUX-00022-run-fix-20260820T105810Z-p67382-n663796000-c1
- **Owner base:** f1056c687c9c5130d802818c738ec20b6bdd8045
- **Owner fingerprint:** -
- **Owner since:** 2026-08-20T10:58:10Z
- **Owner until:** 2026-08-20T12:58:10Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-20T10:57:47Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

Live v4 smoke on quench connected successfully, then the server rejected its first encoded access unit: Intel Hardware H265 Encoder MFT emitted Main profile, Main tier, Level 4.1, while annexb::validate_config required High tier. The probe that informed the HLD used the same 20 Mbit/s rate but its parser skipped the tier bit; Media Foundation exposes no separate HEVC tier setting. Expected: the stated 20 Mbit/s HEVC contract accepts and validates Main tier, while still refusing wrong profile, level, chroma, depth, or dimensions.

## Fix

<unfixed — raised only>

## Notes
