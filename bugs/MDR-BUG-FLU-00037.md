# MDR-BUG-FLU-00037 — Rhydra advertises input and auxiliary ports even when their listeners fail to bind

- **State:** Open
- **Priority:** Should
- **Severity:** High
- **Area:** rhydra/server-lifecycle
- **Raised:** 2026-08-22T19:40:40Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T133202Z-ba991e4f
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00037-run-fix-20260823T133202Z-ba991e4f
- **Owner base:** 223f13661944d5eb3a3ea85f5c1825cddfc2f20c
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T13:32:02Z
- **Owner until:** 2026-08-23T15:32:02Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-22T19:40:40Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

In tools/latency-spike/server/src/win/pipeline.rs:231-271, input and auxiliary listener startup failures are only logged after the video header has already advertised those ports at pipeline.rs:384-390. The video session can therefore look connected while input or auxiliary services are dead. Listener readiness must gate advertisement or terminate the server before a client receives an unusable contract.

## Fix

<unfixed — raised only>

## Notes
