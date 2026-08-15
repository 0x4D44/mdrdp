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

## `ironrdp-graphics` 0.9.0 — RFX luma is scaled by 32, not raw

`reconstruct_to_rgba` converted YCbCr to RGB as `y + 128`, using the coefficient directly.
RFX carries luma scaled by **32** with a DC offset of **4096** (= 128 * 32), so the
conversion must add 4096 and shift the result right by 5.

Without the scale, every value overflows. The DC term still lands in range, so a tile
keeps its average colour — but every detail coefficient clips, so a photograph
reconstructs as a flat block of that average. That is precisely how a Windows wallpaper
rendered: a flat saturated field where the true image is a smooth gradient.

The conversion is now `rfx_ycbcr_to_rgb`, extracted so it can be pinned by a test, and it
mirrors FreeRDP's `general_yCbCrToRGB_16s8u_P3AC4R`
(`libfreerdp/primitives/prim_colors.c`), including its 2^16 fixed-point chroma constants:

```text
Y = (y + 4096) << 16
R = ((Cr * 1.402525 * 2^16 + Y) >> 16) >> 5
G = ((Y - Cb * 0.343730 * 2^16 - Cr * 0.714401 * 2^16) >> 16) >> 5
B = ((Cb * 1.769905 * 2^16 + Y) >> 16) >> 5
```

Measured against FreeRDP 3.27.1 on the same host, same wallpaper, captured back to back:

| | before | after |
|---|---|---|
| mean absolute error vs reference | — (flat field) | **4.3 / 255** |
| luma standard deviation | 59.1 (noise) | 21.4 (reference: 20.9) |
| detail ratio vs reference | — | **1.03** |
| pure black | 16.6% | **0.0%** (reference 0.0%) |

Tests live in `src/gfx.rs` (`rfx_ycbcr_matches_hand_computed_values`,
`a_bright_luma_is_scaled_rather_than_clipped`) with every expected value worked out by
hand from the formula above. Reverting to `+ 128` turns a mid-tone into 255 and makes them
fail, which is what the old code did to every pixel of a photograph.

### Keeping it honest

```
diff -r ~/.cargo/registry/src/*/ironrdp-graphics-0.9.0/src vendor/ironrdp-graphics/src
```

should show only the two `mdrdp patch:` blocks.
