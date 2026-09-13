# MDR-BUG-FLU-00037 — Rhydra advertises input and auxiliary ports even when their listeners fail to bind

- **State:** Fixed
- **Priority:** Should
- **Severity:** High
- **Area:** rhydra/server-lifecycle
- **Raised:** 2026-08-22T19:40:40Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T084808Z-d39b68cb
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00037-run-verify-20260913T084808Z-d39b68cb
- **Owner base:** 751155dd15bc85d7716491f4621816712bbe0df3
- **Owner fingerprint:** sha256:3ca6f3333e2e9fad6c9dd11c708dd393c7be2277a8eea787d3b69728639b9166
- **Owner since:** 2026-09-13T08:48:08Z
- **Owner until:** 2026-09-13T10:48:08Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-22T19:40:40Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T13:38:12Z, deltic:auto role=fix run=fix-20260823T133202Z-ba991e4f branch=task/bug-MDR-BUG-FLU-00037-run-fix-20260823T133202Z-ba991e4f code=e21cada gate=manual)

## Observation

In tools/latency-spike/server/src/win/pipeline.rs:231-271, input and auxiliary listener startup failures are only logged after the video header has already advertised those ports at pipeline.rs:384-390. The video session can therefore look connected while input or auxiliary services are dead. Listener readiness must gate advertisement or terminate the server before a client receives an unusable contract.

## Fix

Session startup now binds the input listener and the optional auxiliary listener
as one synchronous transaction before constructing the video header or listener.
The already-bound sockets move into their service threads, so a client can never
receive an advertised side-channel contract whose bind failed. If the auxiliary
bind fails, the input listener is dropped as the startup error unwinds.

The auxiliary-disabled path still binds input alone and advertises no clipboard.
Portable regression tests cover both successful shapes and all-or-nothing cleanup
when the second bind is occupied.

## Notes
