# MDR-BUG-FLU-00033 — deploy final driver check races PnP version propagation after install

- **State:** Closed
- **Priority:** Must
- **Severity:** Medium
- **Area:** deploy/driver
- **Raised:** 2026-08-22T00:09:13Z
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
- **State history:** Open (2026-08-22T00:09:13Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-22T00:24:59Z, deltic:auto role=fix run=fix-20260822T001032Z-d0709899 branch=task/bug-MDR-BUG-FLU-00033-run-fix-20260822T001032Z-d0709899 code=917c366 gate=manual) -> Closed (2026-09-13T08:45:31Z, 0x4D44/Codex verify run=verify-20260913T083218Z-52835e90)

## Observation

The first v0.1.119 deployment installed and bound IDD 0.3.0.2, but the immediate final PROBE_PS1 sample still returned active_driver_ver 0.3.0.1, so deploy reported failure. A direct present-instance query moments later reported oem104.inf / 0.3.0.2, and an unchanged verify-only deploy then passed. Expected: final activation verification remains strict but polls boundedly through normal PnP/CIM propagation rather than producing a false failure.

## Fix

<unfixed — raised only>

## Notes

## Verification

Independent verification confirmed fix commit 917c366bb7862c230d8e7478edb6e27b62b4e481. The current deploy path uploads and invokes the bounded, read-only DRIVER_STATUS_PS1 polling script in src/deploy.rs:1132-1191 and calls it for final driver verification at src/deploy.rs:1786. The focused regressions deploy::tests::driver_status_wire_contract_is_bounded_read_only_and_parseable and deploy::tests::driver_wait_rejects_stale_absent_ambiguous_and_query_error_evidence each passed. An independent worker also ran the driver_ test family: 10 passed.

To exercise the root termination behavior, I temporarily added an assertion to the existing wire-contract regression requiring if ($matches) { break }. Replacing that production branch with if ($false) { break } made the test fail at the temporary assertion (driver status wait must stop as soon as expected evidence appears, exit 101). The temporary assertion and mutant were removed, and the original focused regression passed again.

The repository gates then passed: cargo build --locked; cargo test --locked; cargo fmt --all -- --check; cargo clippy --all-targets --locked -- -D warnings; ./scripts/test-vendored.sh; and ./scripts/check-windows.sh --locked. The Windows check exited 0 and emitted the repository's existing icon and src/present.rs unused-parameter warnings.

No live Quench deployment or Windows PnP observation was attempted because it would require remote access and state-changing driver installation. The original PnP propagation observation remains the end-to-end product evidence; this pass verifies the bounded polling and strict classifier locally.
