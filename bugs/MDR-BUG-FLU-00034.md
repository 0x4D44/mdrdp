# MDR-BUG-FLU-00034 — native doctor ignores saved favourite host and SSH user

- **State:** Closed
- **Priority:** Should
- **Severity:** Medium
- **Area:** cli/native diagnostics
- **Raised:** 2026-08-22T00:29:31Z
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
- **State history:** Open (2026-08-22T00:29:31Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-25T05:58:12Z, deltic:auto role=fix run=fix-20260825T055227Z-f9c740f9 branch=task/bug-MDR-BUG-FLU-00034-run-fix-20260825T055227Z-f9c740f9 code=92b6ffe gate=manual) -> Closed (2026-09-13T08:45:31Z, 0x4D44/Codex verify run=verify-20260913T083241Z-a495e763)

## Observation

With a saved Quench favourite whose host is quench.lan.example and ssh_user is marti, `mdrdp Quench --doctor` diagnoses the literal host Quench as the process user and fails BatchMode key authentication. The normal session path resolves the same favourite correctly. Expected: read-only native diagnostics resolve a favourite name and apply its saved host and SSH identity with the same flag-over-favourite precedence as a session.

## Fix

<unfixed — raised only>

## Notes

## Verification

Independent verification confirmed fix commit 92b6ffeba72f6f81a8cfb2dd4b68a58cbb6481a4 and the current native diagnostic resolver in src/main.rs:90-103, with doctor wiring at src/main.rs:688-695. The focused regression tests::native_diagnostics_resolve_a_saved_favourite_and_its_ssh_user passed for both saved-favourite resolution and explicit SSH-user precedence. The adjacent tests::ssh_user_flag_beats_favourite_and_absence_defers_to_ssh_config regression also passed.

As a red root mutant, replacing the resolved-favourite branch with (positional.to_owned(), ssh_user.map(str::to_owned)) made the focused regression fail at src/main.rs:2212: left ("Quench", None), right ("quench.lan.example", Some("marti")). The resolver was restored and the focused regression passed again.

The repository gates then passed: cargo build --locked; cargo test --locked; cargo fmt --all -- --check; cargo clippy --all-targets --locked -- -D warnings; ./scripts/test-vendored.sh; and ./scripts/check-windows.sh --locked. The Windows check exited 0 and emitted the repository's existing icon and src/present.rs unused-parameter warnings.

No live mdrdp --doctor or SSH observation was attempted because no remote diagnostic run was available and it would contact a host. The original Quench favourite observation remains the end-to-end product evidence; this pass verifies the resolver and precedence behavior locally.
