# Vendored dependencies

## `ironrdp-session` 0.11.0 — Bandwidth Measure requests are answered

The published crate answers auto-detect RTT requests but logs every Bandwidth Measure
Start/Stop as "not yet implemented" and drops it. Windows runs continuous network
detection ([MS-RDPBCGR] 2.2.14): it brackets bursts of its own traffic with Start/Stop
and expects Bandwidth Measure Results back. A server that never receives Results keeps
re-probing — quench sent **140 Start/Stop pairs in a 40-second session** — and throttles
the EGFX pipeline meanwhile: repaints stop partway (frame ids skip), the session dribbles
at ~2 fps, and input-to-paint latency runs 1–4 s on a 3 ms LAN.

The vendored crate mirrors FreeRDP (`libfreerdp/core/autodetect.c` +
`rdp_recv_tpkt_pdu`): a Start records the time and starts counting **every inbound
frame's bytes** (fast-path included — `ActiveStage::process` feeds each frame length to
`x224::Processor::register_inbound_bytes`), a Stop answers with the elapsed
milliseconds and byte count, response type 0x0003 for a connect-time stop (0x002B) and
0x000B otherwise. A Stop with no Start still gets a zeros response — the server is
blocked on the reply either way.

Measured against quench, same scripted session (open/close Explorer, fullscreen cycle):
before, the post-resize repaint stopped after 3 frames and the session went silent for
33 s; after, the repaint runs to completion (18 frames, sub-second) and responses carry
real numbers (e.g. 44 555 bytes / 7 ms). Tests in `src/x224/mod.rs` (`bandwidth_*`,
`a_stop_without_a_start_*`, `a_connect_time_stop_*`); each was made to fail by
re-breaking the fix before being trusted.

Remove when a released `ironrdp-session` answers bandwidth measure requests.

## `ironrdp-rdpsnd` 0.9.0 — stable negotiated format order

Published IronRDP intersects the server and client format sets through a randomly seeded
`HashSet`, then gives the playback handler only the server's numeric `format_no`. That
number indexes the shuffled client list sent on the wire, so a handler advertising more
than one playable format cannot know which rate and channel count the server selected.

The vendored client preserves the handler's advertised order while filtering it against
the server offer. It also calls the source-compatible `set_negotiated_formats` callback
with the exact list before sending it. mdrdp resolves every Wave2 index only against that
reported list and drops an out-of-range index instead of guessing.

Remove this patch when a released `ironrdp-rdpsnd` both preserves the negotiated wire
order and exposes that exact list to `RdpsndClientHandler`.

## `ironrdp-connector` 0.10.0 — one flag added, smart-card made opt-out

Two deliberate differences from the published crate. First, a **single flag** in
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

### Smart-card logon is behind an off-by-default `scard` feature (2026-08-16)

Upstream builds `sspi` with its `scard` feature unconditionally. That drags
`winscard → flate2/zlib → libz-sys` into every build, and `libz-sys`'s build script
needs a C zlib the `x86_64-pc-windows-msvc` cross-check host does not have — it is what
kept `scripts/check-windows.sh` red. mdrdp never does smart-card logon (out of scope),
so the vendored crate makes it a real feature: `scard = ["sspi/scard", "dep:picky",
"dep:picky-asn1-der", "dep:picky-asn1-x509"]`, default off. The
`Credentials::SmartCard` variant stays in the public API; without the feature the
CredSSP step answers it with an error instead of an identity (`src/credssp.rs`). Both
feature states compile; the password path was proven live against quench the day the
change landed (CredSSP logon, EGFX frames, graceful shutdown).

### Keeping it honest

The only intended differences from the published crate are the flag and the `scard`
feature above. To confirm that:

```
diff -ru ~/.cargo/registry/src/*/ironrdp-connector-0.10.0 vendor/ironrdp-connector
```

A second, separate limitation is **not** patched here: the connector also cannot send an
auto-reconnect cookie, which blocks proving session resumption in P2b. That one needs a
larger change and a decision about whether to carry it — see
`wrk_docs/2026.08.14 - RESEARCH - downstream unknowns for P2b, P2c, P3a and P7.md`.

## `ironrdp-egfx` 0.3.0 — `ResetGraphics` preserves surfaces

The published client clears its private offscreen-surface table whenever it receives
`RDPGFX_RESET_GRAPHICS_PDU`. MS-RDPEGFX 3.3.5.14 says the client must resize the Graphics
Output Buffer; it does not delete offscreen surfaces or bitmap-cache entries. Those have
their own `DeleteSurface` and `EvictCacheEntry` commands.

Windows 11 exercises this distinction immediately. Quench sent 260 `CacheToSurface`
references after a reset without refilling those slots. Clearing mdrdp's cache dropped all
260 regions; retaining the spec-defined state paints them. The vendored client keeps its
surface metadata too, so a later map or delete still reaches mdrdp's handler.

The intended difference from the published crate is the removal of `self.surfaces.clear()`
in `src/client.rs::handle_reset_graphics`. Remove this patch once a released IronRDP version
preserves surfaces across `ResetGraphics`.

### Additional patch: a failed H.264 decode skips the frame, not the session

`decode_avc420` in `src/client.rs` propagated a decoder error as a PDU-processing error,
which tears down the whole channel over one bad access unit. The vendored copy logs a
warning and skips the frame instead; the stale region heals at the next IDR. Remove when
upstream adopts equivalent per-frame resilience.

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

## `ironrdp-graphics` 0.9.0 — glyph hits accept equal-area reshapes

Windows keys the ClearCodec glyph cache by pixel *content*, so a `GLYPH_HIT` legitimately
arrives with a different shape of the same area — measured live against a Windows 11 host
during window drag/resize: 24 hits in 90 s, every one an exact area match (1x6 hit as 2x3,
12x12 as 24x6, 7x5 as 5x7). The strict width/height equality check in
`ClearCodecDecoder::decode_over` rejected them all, leaving stale rectangles on screen.
The vendored copy mirrors FreeRDP's `clear_decompress_glyph_data`: a hit succeeds when the
cached bytes cover the destination pixel count, reinterpreted at the destination shape.
Pinned by `a_glyph_hit_with_a_different_shape_but_equal_area_succeeds`.

## `ironrdp-graphics` 0.9.0 — `ProgressiveDecoder::context_count` test accessor

One additive method exposing how many codec contexts hold tile state, so mdrdp's tests
can prove that a surface delete discards the deleted surface's progressive state (the
contexts map is private). No behavioural change.

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

## `ironrdp-graphics` 0.9.0 — ClearCodec NSCodec subregions are decoded

The published ClearCodec decoder parses `SubcodecId::NsCodec` and then does nothing. The
outer command still returns success, so the client reports zero decode errors while every
NSCodec rectangle keeps stale pixels. Quench uses those rectangles for text and controls in
Windows first-run setup; the result was several grey horizontal bands across an otherwise
correct screen.

`src/clearcodec/nscodec.rs` implements the four-plane header, bounded RLE expansion,
optional chroma subsampling, color-loss recovery, and YCoCg-to-BGRA reconstruction. A
captured 448x448 ClearCodec command was split into residual, band, and subcodec combinations
and decoded through both implementations. Every combination now matches FreeRDP 3.27.1
byte-for-byte. `clearcodec_nscodec_subregion_is_decoded_instead_of_silently_skipped` and
`clearcodec_nscodec_rle_planes_expand_to_the_declared_bitmap` pin the raw and RLE paths.

## `ironrdp-graphics` 0.9.0 — short V-bars are clipped to the current band

A cached short V-bar can be replayed with a new vertical offset against a shorter band.
FreeRDP clips both the leading background and cached pixels to that band's declared height;
`VBarCache::reconstruct_full_vbar` appended the entire cached run and could paint past
`y_end`. The reconstructed entry now always contains exactly `band_height` rows. The
edge case is pinned by `a_short_vbar_replayed_near_the_band_edge_is_clipped_to_the_band`.

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

## ironrdp-graphics: AVC444 combination module (new, 2026-08-16)

`src/avc444.rs` is mdrdp-authored (no upstream equivalent): the MS-RDPEGFX 3.3.8.3
luma/chroma combination for AVC444 and AVC444v2, plus the full-range BT.709 YUV444 to
RGBA conversion with the 2x2 chroma reconstruction (`4*avg - p01 - p10 - p11`,
conditional on a >= 30 delta). Layouts and coefficients transcribed from FreeRDP's
`prim_YUV.c` / `prim_internal.h` and verified byte-exact against the installed
FreeRDP 3.27.1 primitives by `tools/codec-oracles/avc444diff` (720 differential runs,
plus negative controls). One deliberate divergence: rects are combined via absolute
frame coordinates rather than FreeRDP's ROI-relative pointer walks, which are only
phase-correct for aligned rect origins.

## ironrdp-egfx: AVC444/AVC444v2 decode path (new, 2026-08-16)

Upstream forwards `Codec1Type::Avc444`/`Avc444v2` to `on_unhandled_pdu`. The vendored
client decodes them: both sub-streams through the ONE configured `H264Decoder`
sequentially (the Windows encoder produces a jointly-encoded, single-decoder-compatible
pair; FreeRDP decodes it the same way), per-surface persistent `Yuv444Buffer`s, LC
dispatch by equality (the bitflags `LUMA_AND_CHROMA` value is 0 — `.contains()` is
always true), wire rects treated as exclusive despite the `InclusiveRectangle` typing,
and painting exactly the combined region rects (never the PDU destRect). The
`H264Decoder` trait gains `decode_yuv420` (out-param) and `supports_yuv420`; the
capability advertisement is filtered against both, so AVC444 is never advertised
without a YUV-capable decoder. All AVC failures (444 and the 420 arm's
frame-smaller-than-rect case, which upstream escalated into a channel error) now skip
the frame and report through the new `on_decode_failure(codec, reason)` handler seam.
`ironrdp-graphics` became a path dependency so standalone (in-crate) tests compile
against the vendored module rather than crates.io.

## `ironrdp-pdu` 0.9.0 — a restarting server no longer fails the decode (2026-08-17)

`ServerSetErrorInfoPdu::decode` rejected any code its `ErrorInfo` table did not list,
and the table stops at `ERRINFO_SERVER_CSRSS_CRASH` (0x18). MS-RDPBCGR 2.2.5.1.1 defines
two more — **ERRINFO_SERVER_SHUTDOWN (0x19)** "The remote server is busy shutting down"
and **ERRINFO_SERVER_REBOOT (0x1A)** "The remote server is busy rebooting" — and those
are exactly what a host sends on its way down. FreeRDP 3.27.1 is missing them too
(`freerdp/error.h` also stops at 0x18), so this is not an IronRDP-only gap.

Rebooting a live host therefore produced `errorInfo: unexpected info code` and mdrdp
reported an unexplained protocol error, where Microsoft's client says the server is
restarting. Two changes:

- the two codes are added to `ProtocolIndependentCode`, with the spec's own wording;
- an unrecognised code now decodes to a new `ErrorInfo::Unknown(u32)` instead of failing
  the PDU. This PDU is the server *explaining why the session is ending*; rejecting it
  discards the explanation and substitutes a parse error. The next code Microsoft adds
  will be carried, not fatal.

`ErrorInfo::Unknown` round-trips (`as_u32` returns the raw value) and describes itself as
`[Unrecognised error code] 0x…`. Pinned by `disconnect::tests::
a_restarting_host_decodes_instead_of_failing_the_pdu` and
`an_unknown_code_is_carried_rather_than_rejected`, which decode real four-byte wire
payloads; both were made to fail first, by moving `ServerReboot`'s discriminant and by
restoring the reject.

## `ironrdp-session` 0.11.0 — the disconnect reason keeps its code

`GracefulDisconnectReason` flattened a Set Error Info disconnect straight into
`Other(String)`, so a client could print the reason but never branch on it. The vendored
crate adds `GracefulDisconnectReason::ErrorInfo(ErrorInfo)`; `description()` returns the
same text as before, so anything that only logs the reason is unaffected. mdrdp uses the
code in `src/disconnect.rs` to tell "the host is rebooting" from "somebody else took your
session". Remove when upstream exposes the code.
