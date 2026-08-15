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

## `ironrdp-graphics` 0.9.0 — the RFX Progressive upgrade pass was wrong in five ways

`decode_upgrade_pass` and the SRL reader are ported from FreeRDP's
`progressive_rfx_upgrade_component` / `progressive_rfx_upgrade_block` /
`progressive_rfx_srl_read`.

This matters more than it sounds: the server sends **two upgrade passes for every first
pass** (measured on quench: 2040 UPGRADE tiles to 1020 FIRST), so a broken upgrade path
discards two thirds of the picture data and leaves the image at coarse first-pass quality.

The five defects, each confirmed against the C:

1. **The shift omitted the base quantiser.** FreeRDP shifts a refinement by
   `(baseQuant + progQuant) - 1`; we shifted by the progressive term alone, leaving every
   refinement `2^(base-1)` — typically 32x — too small. The first pass was already
   correct, because `dequantize_component_ccq` (`<< q-1`) and `progressive_dequantize`
   (`<< bitPos`) compose to the same total; only the upgrade path was short.
2. **Both bitstreams restarted at every band.** FreeRDP attaches ONE raw stream and ONE
   SRL stream per component and lets all ten bands consume from where the last stopped.
   We re-created both per band, so every band after HL1 re-read HL1's bits.
3. **LL3 was routed through SRL.** FreeRDP clears `nonLL` for LL3 and reads it entirely
   from the raw stream, never consulting the sign array. Routing it through SRL consumed a
   symbol the encoder never wrote and skipped raw bits it did.
4. **SRL `kp` started at 0 instead of 8**, so the first symbol of every component used
   k = 0 instead of k = 1 — a divergence on the very first bit.
5. **SRL magnitudes used the wrong code.** FreeRDP uses a bounded unary count (start at 1,
   stop at `(1 << numBits) - 1`); ours used a Golomb-Rice quotient plus remainder bits,
   giving both a different value and a different bit consumption. The `mode` latch, which
   forces a unary symbol after an escape-signalled short run, was also missing.

Measured on quench, same wallpaper both runs (luma MAE between the two captures: 0.6, so
this is a genuine A/B rather than two different Spotlight images):

| | base quant omitted | corrected |
|---|---|---|
| sharpness (Laplacian variance) | 64.5 | **77.7** |

That is a 20% increase in high-frequency detail — the refinement passes finally
contributing instead of being scaled into irrelevance.

Tests live in `src/gfx.rs` (`srl_*`), hand-traced from the C bit by bit. `SrlReader` is
`pub` purely so they can reach it: the vendored crate is not a workspace member, so its
own `#[cfg(test)]` module cannot be run by `cargo test`.

## `ironrdp-graphics` 0.9.0 — two more, found by the same diff

**The inverse DWT narrowed by truncation, not saturation.** `dwt_extrapolate::t` was
`value as i16`, which wraps: an intermediate one past the rail flips sign, turning a
bright sample into a dark one, and the lifting steps then spread that error into its
neighbours. FreeRDP clamps at every one of these ~21 sites (`clampi16`, progressive.c:591).
Pinned by `the_dwt_narrowing_saturates_rather_than_wrapping` in `src/gfx.rs`; reverting it
turns 32768 into -32768.

**The DWT variant is chosen per REGION.** MS-RDPRFX carries the reduce-extrapolate bit in
the RFX_PROGRESSIVE_REGION flags as well as in CONTEXT, and FreeRDP reads the region's
(`region->flags & RFX_DWT_REDUCE_EXTRAPOLATE`, progressive.c:959 and :1366). We applied
the context's value to every region, so a dissenting region would decode with both the
wrong band layout and the wrong inverse transform. Latent on this server — every region
here agrees with the context — but wrong by construction.

## `ironrdp-pdu` 0.9.0 — ClearCodec short-V-bar yOn/yOff were transposed

`src/codecs/clearcodec/bands.rs`, SHORT_VBAR_CACHE_MISS, read the two fields the wrong way
round:

```rust
let y_on  = first_word >> 6;     // was: bits 13:6
let y_off = first_word & 0x3F;   // was: bits 5:0
```

MS-RDPEGFX 2.2.4.1.1.2.1.1.3 puts **yOn in the low 8 bits** and **yOff in bits 13:8**,
which is how FreeRDP reads it (libfreerdp/codec/clear.c):

```c
vBarYOn  = (vBarHeader & 0xFF);
vBarYOff = ((vBarHeader >> 8) & 0x3F);
```

### Why it mattered so much

Under the old reading `y_on` ranges to 255 while `y_off` caps at 63, so the `yOff < yOn`
validity check fires for **any** yOn above 63 and the tile is rejected outright. Measured
against a live Windows host: 84 of 222 ClearCodec commands failed here directly, and
because the v-bars those tiles would have cached never existed, a further 92 later tiles
failed with "V-bar cache miss on hit" or "glyph cache miss on hit". Roughly 90% of
ClearCodec tiles were lost to this single transposition — text and UI regions simply not
painted.

After the fix that error no longer occurs at all.

Pinned by `clearcodec_short_vbar_takes_y_on_from_the_low_byte` in `src/gfx.rs`, which
decodes a band whose word is `(20 << 8) | 10`. Restoring the transposition makes it fail
with the production error verbatim: `shortVBarYOff < shortVBarYOn`.

## `ironrdp-pdu` 0.9.0 — a one-colour RLEX palette still uses one stop-index bit

`src/codecs/clearcodec/rlex.rs` special-cased `palette_count <= 1` to **zero** stop-index
bits and took a separate parsing path that read one byte per segment. FreeRDP computes
`numBits = CLEAR_LOG2_FLOOR[paletteCount - 1] + 1`, and `CLEAR_LOG2_FLOOR[0]` is 0, so a
one-colour palette gives **numBits = 1** and the normal two-bytes-per-segment path.

Reading a byte at a time produced roughly twice as many segments as the region had room
for, and the tile was rejected with `rlex: suite exceeds region pixel count`. With the fix
a live session decodes 10 ClearCodec commands and 6 progressive commands with **zero**
decode errors, zero undecoded regions and zero surface errors.

Pinned by `rlex_single_entry_palette_reads_two_bytes_per_segment` in `src/gfx.rs`;
restoring the special case turns one segment into two.

## `ironrdp-graphics` 0.9.0 — ClearCodec must composite over the destination, not over black

`ClearCodecDecoder::decode` allocated a zeroed buffer and composited the residual, bands
and subcodec layers into it. Those layers do not have to cover the whole tile: FreeRDP
composites straight into the destination surface, so any pixel a layer does not write
keeps what is already on screen. Decoding over black and then blitting the whole tile
paints BLACK over exactly those pixels.

Measured on a live session: both ClearCodec tiles in a frame decoded to a mean of
(0,0,0) — a black rectangle over the wallpaper — and the logon dialog's translucent panel
rendered as opaque grey blocks around the text and buttons.

`decode_over` takes the destination's current content as the starting buffer;
`decode` still exists and passes `None`, which is the old behaviour. The caller in
`gfx::apply_wire_to_surface1` extracts the destination rect and swaps it to BGRA first —
the surface stores RGBA and this decoder works in BGRA, so seeding without the swap would
leave every uncovered pixel with red and blue exchanged.

## `ironrdp-graphics` 0.9.0 — RFX_TILE_DIFFERENCE was parsed and then ignored

A tile whose flags carry `RFX_TILE_DIFFERENCE` (0x01) holds a **delta** against the
coefficients already retained for it, not a replacement. FreeRDP adds the two —
`prims->add_16s_inplace(buffer, current, ...)` inside `progressive_rfx_dwt_2d_decode`
when `coeffDiff` is set. `TileState::decode_first` overwrote instead, so such a tile threw
away everything the earlier passes had built.

Only a few tiles per frame carry the flag, so this surfaced as **exactly one wrong tile**
on an otherwise perfect screen — in the captured frame, tile (11,0) had `flags=0x01` while
its neighbour had `0x00`, and that single tile rendered about +21 R, +11 G, -5 B off.

Also in this change: the base and progressive dequantisation are applied as ONE shift of
`quant + prog - 1` per band, wrapping, matching `general_lShiftC_16s` (prim_shift.c). They
were two passes with *different* overflow behaviour — the base wrapped, the progressive
saturated — which can only agree while nothing overflows. This produced no observable
change on the captured frames and is kept because it is what the reference does.

### Verifying against FreeRDP without a server or a display

Ground truth comes from FreeRDP's own decoder run on the same bytes:

1. Dump the payloads: temporarily write each `WireToSurface2Pdu::bitmap_data` to a file
   from `gfx::apply_wire_to_surface2`.
2. Decode them with ours: `cargo run --release --bin progcmp -- frame00.bin ...` prints
   the mean colour of each 64px tile in row 0.
3. Decode them with FreeRDP: build a small C harness against the installed library —

```c
PROGRESSIVE_CONTEXT* ctx = progressive_context_new(FALSE);
progressive_create_surface_context(ctx, 0, 1920, 1080);
REGION16 invalid; region16_init(&invalid);
progressive_decompress(ctx, buf, len, dst, PIXEL_FORMAT_BGRX32, 1920 * 4, 0, 0,
                       &invalid, 0, frameId);
```

```
cc -O2 -o refdec refdec.c $(pkg-config --cflags --libs freerdp3)
```

With this fix, all 30 row-0 tiles match FreeRDP's output exactly. That comparison is what
found this bug, after four other hypotheses (upgrade passes, the bitmap cache, tile-state
contamination, and content dependence) had each been tested and eliminated.
