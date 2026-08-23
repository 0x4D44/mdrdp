# MDR-BUG-FLU-00041 — Rhydra 5K HEVC fallback requires Level 4.1, which cannot describe a 5K stream

- **State:** Open
- **Priority:** Should
- **Severity:** High
- **Area:** rhydra/codec
- **Raised:** 2026-08-22T19:40:40Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T135329Z-2d0d21ea
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00041-run-fix-20260823T135329Z-2d0d21ea
- **Owner base:** dcb531ce2a59155f420c54ef895314b4856d94fb
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T13:53:29Z
- **Owner until:** 2026-08-23T15:53:29Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-22T19:40:40Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

tools/latency-spike/server/src/win/pipeline.rs:304-307 creates one 5120x2880 HEVC stream, while annexb.rs:414-435 requires level_idc 123 and encode.rs:997-1001 requests Level 4.1. Level 4.1 cannot describe that 5K picture, so the fallback must either be rejected or advertise an invalid contract. Select and validate a level appropriate to the encoded geometry and rate.

## Fix

Rhydra now selects the smallest HEVC Main-tier level whose H.265 picture-size,
dimension, luma-sample-rate, and bitrate limits cover the requested stream. The
default 2560×1440@60 fallback requests and validates Level 5; 5120×2880@60 requests
and validates Level 6. Contracts beyond Level 6.2 fail before an encoder is opened.

The selected level is applied to the Media Foundation output type, retained through
mid-stream renegotiation checks, and independently enforced against the emitted SPS
before any HEVC access unit reaches the viewer.

## Notes
