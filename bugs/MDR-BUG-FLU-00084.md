# MDR-BUG-FLU-00084 — Deploy retry without a running Rhydra agent loses the retained 5K/200% policy

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** deploy/display
- **Raised:** 2026-08-23T22:19:13Z
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
- **State history:** Open (2026-08-23T22:19:13Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T23:45:30Z, deltic:auto role=fix run=fix-20260824T233341Z-1ba54fdd branch=task/bug-MDR-BUG-FLU-00084-run-fix-20260824T233341Z-1ba54fdd code=db1b0ae8bd7527393db73cc19b63243d31ef73ab gate=manual) -> Closed (2026-09-13T13:43:26Z, 0x4D44/Codex verify run=verify-20260913T133254Z-e33dc7c9)

## Observation

After the failed same-version deploy left RhydraAgent stopped on Quench, retrying the v0.1.163 force deploy had no live agent status from which to recover the desired display. It silently installed defaults of 2560x1440 at 100% although the host was already configured for 5120x2880 at 200%, then failed its display check. Expected: a deploy retry preserves the last installed display policy even when the agent is stopped, rather than depending only on a live status probe. This is a distinct no-agent recurrence beyond fixed MDR-BUG-FLU-00032.

## Fix

`db1b0ae8bd7527393db73cc19b63243d31ef73ab` records the installed
`RhydraAgent` service command during preflight and falls back to its display tuple
when the agent status port is unavailable. A stopped-agent retry therefore passes
the retained 5120x2880/240 Hz/200% policy to the replacement agent install.

## Notes

## Verification

The verification tree was commit `269c3c95dc90e381c834f845ef1bfb0accf77f5b`,
which contains fix commit `db1b0ae8bd7527393db73cc19b63243d31ef73ab`. The fallback
is implemented at `src/deploy.rs:425-431` and used by the install plan at
`src/deploy.rs:722-730`; the service command is gathered by `PROBE_PS1`.

The lead and independent verifier ran
`deploy::tests::stopped_agent_redeploy_preserves_the_installed_service_display_request`
(1/1), and the complete deploy test family passed (40/40). The test supplies a
stopped agent with no status response but a valid installed service command and
asserts the retained display tuple in the generated install action.

As the lead root mutant, I removed the service-command fallback from
`Evidence::effective_display`, leaving only `desired_display`. The regression then
failed at `src/deploy.rs:2161` with an install command missing
`--display 5120 2880 240 200`; restoring the fallback made it pass again. The
independent verifier reproduced the same red mutant, and source diff/checks were
clean after restoration.

The six repository gates all exited 0 on the verification tree: `cargo build
--locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy
--all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and
`./scripts/check-windows.sh --locked`. The Windows check emitted the existing
unused `width`/`height` warnings in `src/present.rs`.

No live Quench deployment was performed: the recorded host was unreachable from
the verification host, and deployment would change remote state. The deterministic
deploy-plan regression and its root mutant verify the fixed behavior at the layer
that selects the retained policy.
