# MDR-BUG-FLU-00127 — Native control replies bypass memory and end-to-end timeout bounds

- **State:** Fixed
- **Priority:** Should
- **Severity:** Medium
- **Area:** native/control-client
- **Raised:** 2026-09-04T22:00:27Z
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
- **State history:** Open (2026-09-04T22:00:27Z, raised via `deltic bugs new`) -> Fixed (2026-09-05T06:38:24Z, deltic:auto role=fix run=fix-20260905T061934Z-52de8fba branch=task/bug-MDR-BUG-FLU-00127-run-fix-20260905T061934Z-52de8fba code=aff1e45 gate=manual)

## Observation

Source-confirmed at baseline `7ef48e989010f7df0474d3ff5d331bb5152561f6`; no app, tests, or hostile peer were run.

Native startup and doctor calls pass a bounded duration to the shared control client (`~/language/mdrdp/src/native/probe.rs:298`, `:310`, `:326`; `~/language/mdrdp/src/native/doctor.rs:228`). However, `query_status` and `send_request` accumulate replies with unrestricted `BufReader::read_line` into a `String` (`~/language/mdrdp/tools/latency-spike/server/src/control.rs:576`, `:628`). Their socket timeout applies to individual reads; it is not an absolute deadline for the line. A faulty or hostile host control endpoint can keep sending newline-free bytes before each read times out, growing memory and keeping the caller inside the helper beyond its advertised probe deadline.

The helper also grants the full supplied duration independently to connect and reading, and does not set a write timeout. The caller cannot enforce its remaining deadline while blocked inside the helper. Expected: a finite response-size ceiling and one end-to-end deadline. Actual by source: neither bound covers the complete exchange.

## Fix

<unfixed — raised only>

## Notes

Proposed fix (small/medium, approximately 2–4 hours): share a bounded control-reply reader, reject excess bytes before allocation grows, and derive connect/write/read timeouts from one absolute deadline. Preserve a clear timeout result at native call sites. Keep the reply ceiling large enough for legitimate status and clipboard-comparison replies.

Future regression criteria: an unterminated oversized reply is rejected after at most limit + 1 bytes; a drip-fed reply cannot renew the total deadline; normal status/ack/error replies still decode; time spent connecting or writing reduces the remaining read budget. These checks were not executed in this review.

Medium severity reflects the authenticated-host trust boundary: this is a faulty/compromised endpoint denial of service, not an unauthenticated Internet attack. Fixed MDR-BUG-FLU-00044 bounded incoming server requests, not these client reply readers. The reliability lens's timeout-overrun observation is consolidated here, not filed twice. No fix made.
