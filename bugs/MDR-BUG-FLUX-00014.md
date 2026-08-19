# MDR-BUG-FLUX-00014 — Session-lost dialog shows IronRDP's raw error chain, rustc and build-machine paths included, as the user-facing explanation

- **State:** Fixed
- **Priority:** Should
- **Severity:** Low
- **Area:** ui
- **Raised:** 2026-08-19T14:35:00Z
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
- **State history:** Open (2026-08-19T14:35:00Z, raised via `deltic bugs new` model=claude-opus-5@high); Fixed (2026-08-19T15:00:00Z, claude on flux, fix commit on task branch — regression tests proven red-then-green, both dialog states confirmed on screen)

## Observation

Noticed while fixing MDR-BUG-FLUX-00013 and confirmed by Arthur: the Session-lost
dialog's explanation is the raw failure string, and for the common case that
string is IronRDP's nested error chain with source locations. Arthur's 2026-08-19
screenshot shows the user being told:

```
RDP connection failed: [payload error @ /rustc/ac68faa20c58cbccd01ee7208bf3b6e93a7d7f96/
library/core/src/ops/function.rs:250] PDU error: [<ironrdp_egfx::client::
GraphicsPipelineClient as ironrdp_dvc::DvcProcessor>::process::{{closure}} @
vendor/ironrdp-egfx/src/client.rs:1230] decode error: […/ironrdp-core-0.2.1/src/
error.rs:101] invalid `Type`: Unknown GFX PDU type
```

Three separate problems in one line.

1. **It is addressed to the wrong reader.** Trait paths, closure names and a
   rustc commit hash tell the person who just lost their desktop nothing they can
   act on. The one phrase that carries meaning — "Unknown GFX PDU type" — is at
   the far end of 450 characters.
2. **It leaks the build machine.** `~/.cargo/registry/src/index.crates.io-…`
   is the path on whoever compiled the binary, not on the user's machine. It is
   not a secret, but it is noise that identifies our build host and means nothing
   anywhere else.
3. **The sentence above it is wrong.** The dialog always says "The connection
   dropped. The session is probably still alive on the host — reconnecting resumes
   it." Nothing dropped here: the link was healthy and *we* aborted because a PDU
   would not parse. The fixed sentence is right for a socket failure and misleading
   for everything else `ConnectError` covers.

Mechanism: `main.rs:1575` builds `EndOutcome::Lost(reason.to_string())` straight
from `session::SessionEnd::Failed(ConnectError)`, and `ui::end_dialog::draw` prints
that string verbatim under a hard-coded explanation. No classification happens
anywhere between the failure and the user.

Not a data-loss or availability defect — the session is over either way — which is
why this is Low. It is a wrong-audience defect: shipped UI text written for the
people who wrote the client.

## Fix

`disconnect::LostSession` classifies a failure into the two things the two readers
need: a `detail` sentence for the user and a `technical` chain for us. `main.rs`
builds it (`from_connect_error` / `from_transport`) instead of calling
`reason.to_string()`, and `EndOutcome::Lost` carries it instead of a bare `String`.

**The sentence keys off the error's variant, never its wording.** Matching on text
would break silently the next time IronRDP or the OS rephrases something, and the
fallback has to stay honest. Each `ConnectError` variant gets its own line, and the io
kinds that mean something different to a user (timed out / closed / refused) get theirs.
"Reconnecting resumes it" is only said where it is true — a refused connection has no
session to resume, and a certificate the client rejected will fail identically on the
next attempt, so neither says it. The `Protocol` case, which is what Arthur hit, is
honestly generic: we cannot say which layer refused without reading the chain, and the
chain is now one click away.

**The chain is shown, not deleted, and cleaned before it is.**
`disconnect::strip_call_sites` drops the bracketed groups that carry an `@ file:line`
— `ironrdp-error`'s call-site markers — and keeps the messages between them. Arthur's
450-character failure becomes `RDP connection failed: PDU error: decode error: invalid
`Type`: Unknown GFX PDU type`, with no rustc hash and no cargo-registry path. A bracket
with no location in it is somebody's message and is left alone; a chain that stripping
would empty falls back to the original, so an upstream format change degrades to ugly
rather than to blank.

**It sits behind a disclosure that starts closed** (`end_dialog::technical_details`),
and the window follows it. `with_resizable(false)` blocks the *user* dragging an edge,
not `request_inner_size` — verified live on macOS — so `EndApp::follow_content` re-sizes
the dialog to whatever the frame laid out, under the same floor and cap `window_size`
uses. The
disclosure's open state is held in `EndApp`, not egui's memory, so the dialog stays a
pure function of what it is told and a test can render both states; its animation is
turned off, or the window would be dragged through a dozen intermediate heights.

Validation: eight regression tests across `disconnect` and `ui::end_dialog`, proven red
under three separate mutations — stripping made a no-op (two tests fail, one on the
`/Users/` leak), the old fixed "The connection dropped" sentence restored (two fail),
and the disclosure forced open (two fail, one naming `"Unknown GFX PDU type"` on screen).
The third mutation's output also caught an unfaithful fixture: the test built
`ConnectError::Protocol` from a string that already carried the `Display` prefix, so
both tests now build the error the way the live path does. Confirmed on screen in both
states, including a chain long enough to force the grow. Full lib suite (630), fmt,
clippy `-D warnings`, and `check-windows.sh` all green.

## Residual

The chain is readable but still ours, not the user's — `invalid `Type`: Unknown GFX PDU
type` means nothing outside this codebase. Making it mean something needs the *typed*
failure (`ironrdp::session::SessionErrorKind` is `Pdu`/`Encode`/`Decode`/`Reason`), which
`ConnectError::Protocol(String)` throws away at six construction sites. That is a
plumbing change worth its own ticket, not a rider on this one.

There is no copy button. Whoever reports a bug has to retype the chain or fetch it from
the log, where `main.rs` writes it in full.

This landed on top of a4f3a13, which was reworking the same two files concurrently —
one 160 px floor and 440/420 widths replacing the mock's per-variant sizes, and
`plain_description` dropping the spec's error-class label. That work wins on sizing:
this branch's own floors and `window_shape` were dropped for its `MIN_HEIGHT` and
`window_width` during the rebase, and the scale-factor slack moved back inside
`measured_height`, where it belongs — a live frame's height needs no allowance, only
the headless estimate does. Its test that the floor never pads a real ending passes
unchanged.

## Notes

The full chain must stay reachable, not be deleted: it is what a bug report needs.
`main.rs:1313` already writes the whole `SessionEnd` to stderr, so the log keeps it
regardless of what the dialog shows.
