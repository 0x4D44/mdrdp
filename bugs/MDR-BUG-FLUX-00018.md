# MDR-BUG-FLUX-00018 — A malformed settings.toml silently falls back to defaults, which turns the clipboard direction gate fully permissive

- **State:** Open
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
- **State history:** Open (2026-08-19T18:33:08Z, raised via `deltic bugs new` model=claude-fable-5@high)

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
