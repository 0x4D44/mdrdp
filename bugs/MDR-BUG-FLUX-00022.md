# MDR-BUG-FLUX-00022 — rhydra HEVC rejects quench's valid Main-tier stream because the contract requires unsupported High tier

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** native-transport
- **Raised:** 2026-08-20T10:57:47Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T073846Z-1d11b877
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLUX-00022-run-verify-20260913T073846Z-1d11b877
- **Owner base:** be623e0353270c6ea74aaa94926e79fb21dc709d
- **Owner fingerprint:** sha256:478c981b548abf9d7fd291619fd58eddf5357085f5286ce50c554e048bd74670
- **Owner since:** 2026-09-13T07:38:46Z
- **Owner until:** 2026-09-13T09:38:46Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-20T10:57:47Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-20T11:02:02Z, deltic:auto role=fix run=fix-20260820T105810Z-p67382-n663796000-c1 branch=task/bug-MDR-BUG-FLUX-00022-run-fix-20260820T105810Z-p67382-n663796000-c1 code=2fdfdf2 gate=manual)

## Observation

Live v4 smoke on quench connected successfully, then the server rejected its first encoded access unit: Intel Hardware H265 Encoder MFT emitted Main profile, Main tier, Level 4.1, while annexb::validate_config required High tier. The probe that informed the HLD used the same 20 Mbit/s rate but its parser skipped the tier bit; Media Foundation exposes no separate HEVC tier setting. Expected: the stated 20 Mbit/s HEVC contract accepts and validates Main tier, while still refusing wrong profile, level, chroma, depth, or dimensions.

## Fix

<unfixed — raised only>

## Notes
