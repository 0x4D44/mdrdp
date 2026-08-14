# Vendored dependencies

## `ironrdp-connector` 0.10.0 — one line added

An unmodified copy of the published crate plus a **single flag**, in
`src/connection.rs` where the client's early capability flags are assembled:

```rust
| ClientEarlyCapabilityFlags::SUPPORT_DYN_VC_GFX_PROTOCOL
```

### Why

Without it the RDP server **never offers the graphics dynamic virtual channel**, so EGFX
cannot open and no graphics can ever be decoded. This is not a preference — it is the
difference between a working client and a black screen.

Measured against `temper` (Windows 11 Pro):

| | stock crate | with this one line |
|---|---|---|
| `drdynvc` static channel joined | yes | yes |
| EGFX capabilities confirmed | **never** | `V10_7 { SMALL_CACHE \| AVC_DISABLED }` |
| surfaces created | 0 | 2 |
| frames completed | 0 | 28 |
| codecs received | none | ClearCodec × 39 |

The flag is defined in `ironrdp-pdu` as `SUPPORT_DYN_VC_GFX_PROTOCOL = 0x0100` and is
simply never referenced anywhere in `ironrdp-connector` — `grep` across the crate returns
nothing. FreeRDP does advertise it, which is why FreeRDP gets a graphics channel from the
same host and we did not.

There is no non-patch route: the flag set is assembled inside `create_gcc_blocks`, and
neither `Config` nor `ClientConnector` exposes any way to influence it.

### This is temporary

It belongs upstream. Delete this directory and the `[patch.crates-io]` entry in the root
`Cargo.toml` the moment a released `ironrdp-connector` sets the flag (or exposes a knob
for it).

### Keeping it honest

The only intended difference from the published crate is the flag above. To confirm that:

```
diff -ru ~/.cargo/registry/src/*/ironrdp-connector-0.10.0 vendor/ironrdp-connector
```

A second, separate limitation is **not** patched here: the connector also cannot send an
auto-reconnect cookie, which blocks proving session resumption in P2b. That one needs a
larger change and a decision about whether to carry it — see
`wrk_docs/2026.08.14 - RESEARCH - downstream unknowns for P2b, P2c, P3a and P7.md`.
