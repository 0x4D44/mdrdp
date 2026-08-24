# MDR-BUG-FLU-00089 — LC2-first AVC444 sequence seeds a fake chroma average and suppresses later LC1 detail

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/avc444
- **Raised:** 2026-08-24T08:33:16Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260824T083347Z-fde8063d
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00089-run-fix-20260824T083347Z-fde8063d
- **Owner base:** 52b66eeec2c25f1730a8c950b052d4e59e66f620
- **Owner fingerprint:** -
- **Owner since:** 2026-08-24T08:33:47Z
- **Owner until:** 2026-08-24T10:33:47Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T08:33:16Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

Yuv444Buffer initializes chroma_confirmed_avg to neutral 128 and mark_chroma_seen records that value when LC2 arrives before any luma. A following LC1 frame with a real main-plane average can therefore mark valid LC2 chroma stale and flatten odd-position detail. Reproduce with a full-frame apply_chroma_v2 first, then uniform LC1 luma whose U/V average differs from 128; the odd pixel must retain its auxiliary chroma detail.

## Fix

<unfixed — raised only>

## Notes
