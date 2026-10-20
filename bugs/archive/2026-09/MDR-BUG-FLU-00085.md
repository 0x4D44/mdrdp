# MDR-BUG-FLU-00085 — Deploy's legacy Rhydra uninstaller does not stop the current Windows service

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** deploy/rhydra
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
- **State history:** Open (2026-08-23T22:19:13Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T23:54:48Z, deltic:auto role=fix run=fix-20260824T234549Z-b5e9422b branch=task/bug-MDR-BUG-FLU-00085-run-fix-20260824T234549Z-b5e9422b code=7ed0437ec2c5c513c70ca2b9a3a7638bfe7093c1 gate=manual) -> Closed (2026-09-13T13:43:26Z, 0x4D44/Codex verify run=verify-20260913T133307Z-8ca9d852)

## Observation

On Quench during a same-version v0.1.163 force deploy, the pre-copy uninstaller selected the legacy flat C:\mdrdp\rhydra-agent.exe. It acknowledged worker shutdown but did not know about the installed RhydraAgent service, which immediately relaunched the worker; deploy then failed its quiescence check before copying. Expected: deploy stops the installed service through the current agent or SCM before replacing artifacts, regardless of which legacy binary path is present.

## Fix

`7ed0437ec2c5c513c70ca2b9a3a7638bfe7093c1` makes deploy select the executable
registered in the `RhydraAgent` service command before falling back to a legacy
binary path. The service-aware executable's uninstall path calls `sc.exe stop
RhydraAgent` and waits for the service to reach `STOPPED` before replacement.

## Notes

## Verification

The verification tree was commit `269c3c95dc90e381c834f845ef1bfb0accf77f5b`,
which contains fix commit `7ed0437ec2c5c513c70ca2b9a3a7638bfe7093c1`. Deploy selects
the service executable at `src/deploy.rs:479-489`; the service-aware uninstall path
is `tools/latency-spike/server/src/bin/agent.rs:897-906`.

The lead and independent verifier ran
`deploy::tests::quiesce_prefers_the_installed_service_agent_over_a_legacy_flat_binary`
(1/1), and the complete deploy test family passed (40/40). The regression supplies
both the legacy flat binary and the installed service command and asserts that the
quiesce action invokes the versioned service executable.

As the lead root mutant, I changed `quiesce_actions` to ignore the service command
and always use the legacy `agent_exes` path. The regression failed at
`src/deploy.rs:2074`, showing the flat executable instead of the versioned service
executable; restoring service-command selection made it pass again. The independent
verifier reproduced the same red mutant, and source diff/checks were clean after
restoration.

The six repository gates all exited 0 on the verification tree: `cargo build
--locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy
--all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and
`./scripts/check-windows.sh --locked`. The Windows check emitted the existing
unused `width`/`height` warnings in `src/present.rs`.

No live Quench deployment was performed: the recorded host was unreachable from
the verification host, and deployment would change remote state. The deterministic
quiesce-plan regression and its root mutant verify the fixed service selection at
the layer that controls which uninstaller runs.
