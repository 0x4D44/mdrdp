# MDR-BUG-FLUX-00014 — Session-lost dialog shows IronRDP's raw error chain, rustc and build-machine paths included, as the user-facing explanation

- **State:** Open
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
- **State history:** Open (2026-08-19T14:35:00Z, raised via `deltic bugs new` model=claude-opus-5@high)

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

<unfixed — raised only>

## Notes

The full chain must stay reachable, not be deleted: it is what a bug report needs.
`main.rs:1313` already writes the whole `SessionEnd` to stderr, so the log keeps it
regardless of what the dialog shows.
