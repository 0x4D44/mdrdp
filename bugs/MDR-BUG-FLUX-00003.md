# MDR-BUG-FLUX-00003 — macOS session process UAF: session menu bar dropped before epilogue while still installed in NSApp; panics in muda icon code

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** window
- **Raised:** 2026-08-16T20:44:33Z
- **Discovery source:** Human
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
- **State history:** Open (2026-08-16T20:44:33Z, raised via `deltic bugs new` model=claude-fable-5) -> Fixed (2026-08-24T08:17:00Z, deltic:auto role=fix run=fix-20260824T075750Z-555dee06 branch=task/bug-MDR-BUG-FLUX-00003-run-fix-20260824T075750Z-555dee06 code=8e065fa3e5265203e5a4a379c6ce8a43df5bb8fa gate=manual) -> Closed (2026-09-13T05:10:09Z, independent verifier: AppKit menu-lifetime probe passed and the root detach mutant failed its expected assertion, model=codex@max)

## Observation

Observed live 2026-08-16 (v0.1.29): after a crucible session ended gracefully, the session process panicked on thread 'main' at muda-0.19.3/src/platform_impl/macos/icon.rs:34 — png write_header unwrap on Err(Format(ZeroWidth)) — killing the process and skipping the epilogue dialog.

Root cause (mechanism confirmed by code inspection; the exact garbage read is inference): mdrdp never constructs a muda Icon anywhere, so a 0-width icon cannot exist legitimately. SessionApp owns the SessionMenuBar (src/window.rs:810); app is a local in SessionWindow::run(), so the muda Menu drops when run() returns the event loop for the session-ended epilogue. muda's macOS Menu::drop does NOT remove the menu from NSApp — the NSMenu stays installed as the main menu, and each NSMenuItem holds a raw *const MenuChild ivar pointing at the freed Rust objects. The epilogue then runs on the same NSApp; any menu touch (click, key-equivalent validation, menuNeedsUpdate) dereferences freed memory. Garbage read as MenuChild.icon = Some(0-width icon) reaches PlatformIcon::to_png and panics. This is a use-after-free: a panic tonight, potentially a segfault or silent corruption another night.

The doc comment at src/window.rs:1563 ("dropping this removes the native menu") is wrong on macOS.

Fix direction: keep the SessionMenuBar alive as long as the NSApp can show it — hand it back out of run() alongside the event loop (main.rs already has a surviving slot for epilogue state), or call Menu::remove_for_nsapp before dropping. The launcher's LauncherMenu lives for the app lifetime and is not affected.

## Fix

The independent verifier ran the main-thread probe from the integrated fix commit
`62e48bad8abff3dca0452ff50dbb09a2688a213d` with `MDRDP_GUI_TESTS=1`; it exited 0
after observing the Session menu before close and its absence after teardown. Replacing
only `SessionMenuBar::detach`'s `remove_for_nsapp()` call with a no-op made the same
selected harness fail at `tests/macos_menu_lifetime.rs:56` with the expected
"the ended session must detach its menu" assertion. The exact call was restored and
the source diff is empty. The original macOS session-ended UAF observation is therefore
covered by the direct AppKit lifetime oracle. The repository library gate also passed
with 908 tests and no failures; no live RDP session was needed for this GUI lifetime
regression.

## Notes
