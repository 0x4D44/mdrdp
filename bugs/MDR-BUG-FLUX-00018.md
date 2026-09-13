# MDR-BUG-FLUX-00018 — A malformed settings.toml silently falls back to defaults, which turns the clipboard direction gate fully permissive

- **State:** Closed
- **Priority:** Should
- **Severity:** Medium
- **Area:** settings
- **Raised:** 2026-08-19T18:33:08Z
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
- **State history:** Open (2026-08-19T18:33:08Z, raised via `deltic bugs new` model=claude-fable-5@high) -> Fixed (2026-08-25T05:45:08Z, deltic:auto role=fix run=fix-20260825T054444Z-13d987ad branch=task/bug-MDR-BUG-FLUX-00018-run-fix-20260825T054444Z-13d987ad code=a300ec1 gate=manual) -> Closed (2026-09-13T07:35:22Z, 0x4D44/Codex verify run=verify-20260913T072211Z-70bd8477)

## Observation

`Settings::load_from` correctly returns `Err(Malformed)` when `settings.toml`
does not parse — but the caller (`src/main.rs:723`) answers that with a warning
on stderr and `Settings::default()`.

For most settings that is a reasonable trade. For the **clipboard direction gate
it fails in the wrong direction**: the default is
`ClipboardDirection::Both`, so a user who has deliberately narrowed or disabled
clipboard sharing silently gets it fully re-enabled, in both directions, by a
typo anywhere in the file.

### How I hit it

Writing an acceptance test I set `direction = "from-remote"`. The enum is
`#[serde(rename_all = "snake_case")]`, so the correct spelling is `from_remote`
and the file failed to parse. mdrdp printed one warning line and ran with
clipboard sharing fully on. The test then reported the clipboard as *broken*
(it sent when it should not have), when in fact the gate had been silently
widened.

A GUI user never sees stderr at all.

### Why it matters beyond my typo

The trigger does not need to be a user error. Any of these produce the same
silent widening:

- a field renamed or removed in a later version, read by an older config;
- a partially-written file after a crash or a full disk;
- hand-editing, which the file exists to allow.

The failure is silent, permissive, and applies to a control whose entire purpose
is to stop clipboard content leaving the machine.

### Suggested direction (not a decision)

The narrow fix is that a parse failure should keep **restrictive** defaults for
the direction gate specifically — `Off`, or the last known-good value — rather
than the permissive one, while everything else may still default. The general
version is that any setting which is a *restriction* should fail closed.

Surfacing it beyond stderr is a separate, smaller improvement: a GUI user gets
no signal at all today. `--doctor` or the Settings pane could say "your
settings file did not parse; defaults are in force".

## Fix

<unfixed — raised only>

## Notes

## Verification

Independent verification confirmed fix commit `a300ec1cb617dc43ad13928fc6a9acfdec9e840b` and the fail-closed helper in `src/main.rs:81`, which sets malformed-settings clipboard direction to `Off`; the load-error path calls it at `src/main.rs:787`. The focused regression `tests::malformed_settings_fail_closed_for_clipboard_sharing` passed after restoration. As a red root mutant, changing `ClipboardDirection::Off` to `ClipboardDirection::Both` made it fail at `src/main.rs:2204` with `left: Both`, `right: Off`. The line was restored and the focused regression passed again.

The repository gates then passed: `cargo build --locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy --all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and `./scripts/check-windows.sh --locked`.

No fresh live GUI/RDP validation was available; no new live claim is made. The original malformed-settings observation remains the end-to-end product observation.
