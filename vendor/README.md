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

## `ironrdp-graphics` 0.9.0 — RFX Progressive `quality` is a level, not an index

An unmodified copy of the published crate plus one change, applied identically to the
`TILE_FIRST` and `TILE_UPGRADE` arms of `decode_tile_block` in `src/progressive.rs`.

Upstream reads the tile's progressive quantisation like this:

```rust
let pq_idx = usize::from(tile.quality);          // <- quality used as a table index
if pq_idx >= prog_quant_vals.len() { return Err(InvalidQuantIndex { .. }); }
```

### Why

`quality` is a quality **level**, not an index. `ironrdp-pdu`'s own doc comment on the
field says so: *"`quality` ranges from 0 (minimum) to 0xFF (full quality / no extra
quantization)"*. A tile sent at full quality therefore carries `quality = 0xFF`, and the
server has no reason to send a progressive quant table at all — so upstream looks up
index 255 in an empty table and rejects the whole region:

```
quant index 255 exceeds table length 0
```

The `TILE_SIMPLE` arm three branches earlier already gets this right, using
`ComponentCodecQuant::LOSSLESS` with the comment *"no progressive refinement"*. This patch
makes `First` and `Upgrade` agree with it, and still rejects a genuinely out-of-range
index.

Measured against `quench` (Windows 11), with the same session and wallpaper:

| | stock crate | with this patch |
|---|---|---|
| progressive regions left unpainted | 4 of 6 | **0 of 6** |
| pure black in the presented frame | 85.4% | 16.6% |
| mean RGB vs the reference | distance 119 | distance 90 |

The reference is FreeRDP 3.27.1 against the same host and desktop, captured with
`--screenshot` on our side and `screencapture` on FreeRDP's.

### Still wrong, and not fixed here

With this patch every region decodes, but the reconstruction is still not correct: the
wallpaper comes out as flat saturated blue rather than the reference's smooth gradient.
The DC term survives and the detail does not, which points at the inverse DWT / subband
reconstruction rather than at quantisation. Ruled out already: the context-level and
region-level `reduce_extrapolate` flags (both `true` here, and correctly propagated into
`TileState`), and tile placement. The next step is a comparison against FreeRDP's
`progressive.c`, which decodes this exact stream correctly.

### Keeping it honest

```
diff -r ~/.cargo/registry/src/*/ironrdp-graphics-0.9.0/src vendor/ironrdp-graphics/src
```

should show only the two `mdrdp patch:` blocks.
