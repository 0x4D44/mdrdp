# MDR-BUG-FLU-00041 — Rhydra 5K HEVC fallback requires Level 4.1, which cannot describe a 5K stream

- **State:** Open
- **Priority:** Should
- **Severity:** High
- **Area:** rhydra/codec
- **Raised:** 2026-08-22T19:40:40Z
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
- **State history:** Open (2026-08-22T19:40:40Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

tools/latency-spike/server/src/win/pipeline.rs:304-307 creates one 5120x2880 HEVC stream, while annexb.rs:414-435 requires level_idc 123 and encode.rs:997-1001 requests Level 4.1. Level 4.1 cannot describe that 5K picture, so the fallback must either be rejected or advertise an invalid contract. Select and validate a level appropriate to the encoded geometry and rate.

## Fix

<unfixed — raised only>

## Notes
