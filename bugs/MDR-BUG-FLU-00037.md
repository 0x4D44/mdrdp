# MDR-BUG-FLU-00037 — Rhydra advertises input and auxiliary ports even when their listeners fail to bind

- **State:** Open
- **Priority:** Should
- **Severity:** High
- **Area:** rhydra/server-lifecycle
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

In tools/latency-spike/server/src/win/pipeline.rs:231-271, input and auxiliary listener startup failures are only logged after the video header has already advertised those ports at pipeline.rs:384-390. The video session can therefore look connected while input or auxiliary services are dead. Listener readiness must gate advertisement or terminate the server before a client receives an unusable contract.

## Fix

<unfixed — raised only>

## Notes
