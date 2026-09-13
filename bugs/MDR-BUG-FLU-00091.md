# MDR-BUG-FLU-00091 — EGFX CreateSurface can force a multi-gigabyte client allocation

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/surface-limits
- **Raised:** 2026-08-24T09:25:48Z
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
- **State history:** Open (2026-08-24T09:25:48Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T09:43:53Z, deltic:auto role=fix run=fix-20260824T092607Z-207318fd branch=task/bug-MDR-BUG-FLU-00091-run-fix-20260824T092607Z-207318fd code=d37f299 gate=manual) -> Closed (2026-09-13T14:49:44Z, 0x4D44/Codex verify run=verify-20260913T144045Z-2107283f)

## Observation

The EGFX client rejects only zero-sized CreateSurface PDUs, then the mdrdp surface callback allocates width*height*4 bytes directly. A maximum 65535x65535 request asks for about 17.2 GiB and can panic or abort the client before any bitmap arrives. Reject surfaces whose dimensions or total RGBA bytes exceed the negotiated/display safety limit, before invoking the allocation callback, with a no-allocation regression.

## Fix

`vendor/ironrdp-egfx/src/client.rs:966-1007` now tracks live surface pixels and rejects a CreateSurface before the allocation callback when the 64-Mpixel aggregate budget would be exceeded, including same-ID replacement and deletion accounting (`673f318`, integrated by `d37f299`).

## Verification

Independent verifier ran these focused tests with `deltic timeout 300 cargo test --manifest-path vendor/ironrdp-egfx/Cargo.toml --locked --lib <test> </dev/null`: `oversized_surface_is_rejected_before_the_allocation_callback`, `protocol_legal_skinny_surface_is_not_rejected_by_monitor_limits`, and `live_surface_memory_budget_accounts_for_id_replacement`; each passed 1 test. Replacing the allocation guard polarity at `vendor/ironrdp-egfx/src/client.rs:976` made all three tests fail at their meaningful assertions; restoration made all three pass again, with a clean diff.

Lead verifier on tree `1772165963b2a06aeb36a034f149dbaae0330e00` reran the same three tests; each selected test ran 1 test and passed. The same guard mutant failed with callback count `1` instead of `0` at line 1991, rejected a legal skinny surface at line 2005, and violated the aggregate budget assertion at line 2026; restoring the guard made all three tests pass again.

All repository gates passed: `cargo build --locked`, `cargo test --locked`, `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`, `./scripts/test-vendored.sh`, and `./scripts/check-windows.sh --locked`. Windows checking emitted only the existing unused `width` and `height` warnings at `src/present.rs:95`. No live RDP session was needed because the deterministic CreateSurface handler tests exercise callback suppression and budget accounting directly.
