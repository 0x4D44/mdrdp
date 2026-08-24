//! Increment 3 (HLD §6b) — the previous-frame pixel diff, on the source's device.
//!
//! ## Why this exists
//!
//! Both capture sources report change metadata that is *not* the true per-frame
//! delta on exactly the frames the latency number is about. Duplication inherits
//! DWM's post-idle full recompose (7.2 MB claimed for a 2.5 KB change); the IddCx
//! pool publishes buffer-relative damage (p50 7.25 MB claimed for one caret cell).
//! An isolated keystroke — the frame a user is actually waiting on — therefore
//! misses [`super::pipeline::takes_fast_path`] and pays the whole ~23 ms codec
//! chain. So on those frames we stop trusting the claim and measure the delta.
//!
//! ## Why the diff's answer outranks the metadata
//!
//! §6b, "Trust and the exactness chain": a diff-produced rect set is the measured
//! difference between the two consecutive frames the viewer actually saw — the one
//! it is painting on top of, and the one being sent. Metadata is a *claim* about
//! that difference, made by a component with its own notion of what "since" means.
//! A measurement of the real thing is strictly stronger evidence than a claim about
//! it, so diff rects feed the same fast path, under the same `frame_seq`, and the
//! viewer's invariant needs no change at all.
//!
//! That chain only holds while `prev` really is the frame the viewer last saw,
//! which is why [`PixelDiff::retain`] is **unconditional whenever the diff is
//! enabled** — every consumed frame is retained, hit or miss, so the baseline never
//! silently skips one. It is also why a rebuilt source must
//! [`PixelDiff::invalidate`]: after a rebuild the retained texture predates a
//! discontinuity, and a diff against it would measure against a baseline that was
//! never on screen. The copy is GPU-to-GPU (`CopyResource`, ~0.2–0.5 ms, no CPU),
//! which is cheap enough that a conditional copy would buy less than it costs in
//! ways to be wrong.

use super::Result;
use crate::diff;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BOX, D3D11_CPU_ACCESS_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

/// Retains the previous frame for viewer bootstrap and measures deltas against it.
///
/// Three textures, all desktop-sized and all created on first use: one
/// `D3D11_USAGE_DEFAULT` copy of the previous frame (GPU-resident, never mapped),
/// and two staging surfaces the diff maps — one per side of the comparison. Full
/// desktop size for the same reason [`super::source::RectReadback`] is: a surface
/// sized to the scanned region would be recreated whenever the region grew, and
/// creating a texture costs far more than copying into a corner of an existing one.
/// Copying at the region's own desktop coordinates then keeps source and
/// destination coordinates identical, so there is no offset arithmetic to get
/// wrong — and the packed rects come out in desktop coordinates already.
///
/// The GPU copy is allocated after the first admitted frame because a reconnect must
/// paint a static desktop without waiting for another compositor event. The two CPU
/// staging surfaces remain lazy, so a run that never diffs does not pay for them.
pub struct PixelDiff {
    width: u32,
    height: u32,
    /// The last consumed frame, GPU-to-GPU. `None` until the first `retain`.
    prev: Option<ID3D11Texture2D>,
    /// Whether `prev` is a baseline the viewer actually saw. Cleared on a source
    /// rebuild, where the retained frame straddles a discontinuity.
    prev_valid: bool,
    /// Staging for the current frame's scanned region. The diff packs its output
    /// rects straight out of this map — the §6b "no second readback" property.
    staging_cur: Option<ID3D11Texture2D>,
    /// Staging for the same region of `prev`.
    staging_prev: Option<ID3D11Texture2D>,
}

impl PixelDiff {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            prev: None,
            prev_valid: false,
            staging_cur: None,
            staging_prev: None,
        }
    }

    /// Whether a diff would have a trustworthy baseline to measure against.
    pub fn valid(&self) -> bool {
        self.prev_valid
    }

    /// Drop the baseline without dropping the texture: the pixels are stale, but
    /// the allocation is still the right size for the next frame.
    pub fn invalidate(&mut self) {
        self.prev_valid = false;
    }

    /// The last admitted desktop frame, when it does not predate a source rebuild.
    pub fn retained(&self) -> Option<ID3D11Texture2D> {
        if self.prev_valid {
            self.prev.clone()
        } else {
            None
        }
    }

    /// `prev := texture`, GPU-to-GPU, and mark the baseline usable.
    ///
    /// Called inside the frame's validity window, on every consumed frame while the
    /// diff is enabled — see the module docs for why it is unconditional.
    pub fn retain(
        &mut self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        texture: &ID3D11Texture2D,
    ) -> Result<()> {
        if self.prev.is_none() {
            self.prev = Some(create_texture(
                device,
                self.width,
                self.height,
                Usage::Default,
            )?);
        }
        let prev = self.prev.as_ref().ok_or("previous-frame texture missing")?;
        // SAFETY: both textures are live and share format, size and mip/array
        // layout, which is `CopyResource`'s whole precondition. Neither is mapped
        // here: `prev` is never mapped at all, and the staging copies the diff maps
        // are separate surfaces.
        unsafe { context.CopyResource(prev, texture) };
        self.prev_valid = true;
        Ok(())
    }

    /// Measure the delta between `texture` and the retained previous frame over
    /// `region`, and pack it as wire-ready rects.
    ///
    /// `Ok(None)` is a **miss**, on either of the two ways the answer is not worth
    /// sending: the change coalesced into more than `max_rects` rects, or the packed
    /// pixels would exceed `max_bytes`. Both mean the raw copy costs more than the
    /// encode it would be avoiding, so the frame takes the codec path it was on
    /// anyway — the fast path is an overlay, never a branch.
    ///
    /// `Ok(Some(rects))` is the complete measured delta inside `region`, possibly
    /// empty (the claim was there, the pixels did not move).
    ///
    /// Wire coordinates are `u16`, so the caller must have established that the
    /// desktop fits; the `debug_assert` below states that contract rather than
    /// re-deriving it per rect.
    pub fn diff(
        &mut self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        texture: &ID3D11Texture2D,
        region: diff::Region,
        max_rects: usize,
        max_bytes: u64,
    ) -> Result<Option<Vec<crate::rects::Rect>>> {
        if !self.prev_valid || region.w == 0 || region.h == 0 {
            // No baseline, or nothing to scan. Not a miss in the predicate sense —
            // there is simply no measured delta to report.
            return Ok(Some(Vec::new()));
        }
        if self.staging_cur.is_none() {
            self.staging_cur = Some(create_texture(
                device,
                self.width,
                self.height,
                Usage::Read,
            )?);
        }
        if self.staging_prev.is_none() {
            self.staging_prev = Some(create_texture(
                device,
                self.width,
                self.height,
                Usage::Read,
            )?);
        }
        let prev = self.prev.as_ref().ok_or("previous-frame texture missing")?;
        let staging_cur = self
            .staging_cur
            .as_ref()
            .ok_or("current staging texture missing")?;
        let staging_prev = self
            .staging_prev
            .as_ref()
            .ok_or("previous staging texture missing")?;

        let box_ = D3D11_BOX {
            left: region.x,
            top: region.y,
            front: 0,
            right: region.x + region.w,
            bottom: region.y + region.h,
            back: 1,
        };
        // SAFETY: all four textures are live, share the BGRA8 format and the desktop
        // size, and `box_` is inside every one of them — the caller clipped the
        // region to the desktop these surfaces were created at. Both copies are
        // same-coordinate, so neither destination can overrun.
        unsafe {
            context.CopySubresourceRegion(
                staging_cur,
                0,
                region.x,
                region.y,
                0,
                texture,
                0,
                Some(&box_ as *const D3D11_BOX),
            );
            context.CopySubresourceRegion(
                staging_prev,
                0,
                region.x,
                region.y,
                0,
                prev,
                0,
                Some(&box_ as *const D3D11_BOX),
            );
        }

        let mut cur_map = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: `staging_cur` is live, subresource 0 is its only one (MipLevels
        // and ArraySize are both 1), and `cur_map` is a live local for the call.
        unsafe { context.Map(staging_cur, 0, D3D11_MAP_READ, 0, Some(&mut cur_map)) }?;

        let mut prev_map = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: as above, on the other staging surface.
        let second =
            unsafe { context.Map(staging_prev, 0, D3D11_MAP_READ, 0, Some(&mut prev_map)) };
        if let Err(e) = second {
            // The first mapping must not survive this early return: a leaked map
            // wedges the device for every later frame.
            // SAFETY: exactly one `Unmap` for the successful `Map` above.
            unsafe { context.Unmap(staging_cur, 0) };
            return Err(e.into());
        }

        // Nothing between here and the two `Unmap`s may fail, return early or
        // panic — the diff and the packing only read, allocate and memcpy. That is
        // what keeps the mappings from leaking without a guard type.
        let cur_pitch = cur_map.RowPitch as usize;
        let prev_pitch = prev_map.RowPitch as usize;
        // SAFETY: each mapping covers `height` rows of its own `RowPitch` bytes, so
        // these lengths are exactly the mapped extent. Both slices are dropped
        // before the matching `Unmap` below.
        let cur = unsafe {
            std::slice::from_raw_parts(cur_map.pData as *const u8, cur_pitch * self.height as usize)
        };
        // SAFETY: as above, for the previous frame's mapping.
        let prev_pixels = unsafe {
            std::slice::from_raw_parts(
                prev_map.pData as *const u8,
                prev_pitch * self.height as usize,
            )
        };

        let outcome = match diff::diff_rects(
            cur,
            cur_pitch,
            prev_pixels,
            prev_pitch,
            self.width,
            self.height,
            region,
            max_rects,
        ) {
            // More rects than the fast path carries: a miss, exactly as an
            // over-count metadata claim would be.
            None => None,
            Some(regions) => {
                let bytes: u64 = regions
                    .iter()
                    .map(|r| u64::from(r.w) * u64::from(r.h) * 4)
                    .sum();
                if bytes > max_bytes {
                    // The change is real but large: the raw copy would cost more
                    // than the encode it is meant to pre-empt.
                    None
                } else {
                    Some(
                        regions
                            .iter()
                            .map(|r| {
                                debug_assert!(
                                    r.x + r.w <= u16::MAX as u32 && r.y + r.h <= u16::MAX as u32,
                                    "rect {r:?} does not fit the u16 wire fields; \
                                     the caller gates on desktop size"
                                );
                                crate::rects::Rect {
                                    x: r.x as u16,
                                    y: r.y as u16,
                                    w: r.w as u16,
                                    h: r.h as u16,
                                    pixels: diff::pack_rect(cur, cur_pitch, *r),
                                }
                            })
                            .collect(),
                    )
                }
            }
        };

        // SAFETY: exactly one `Unmap` per successful `Map` above, same subresources.
        unsafe {
            context.Unmap(staging_prev, 0);
            context.Unmap(staging_cur, 0);
        }
        Ok(outcome)
    }

    /// Test one bounded vertical translation against the retained frame.
    ///
    /// This uses the same lazy staging pair as [`Self::diff`]. The returned
    /// remainder is coverage only; the normal routing planner decides raw versus
    /// video after the mapping is released.
    #[allow(clippy::too_many_arguments)]
    pub fn infer_vertical_move(
        &mut self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        texture: &ID3D11Texture2D,
        region: diff::Region,
        damage: &[diff::Region],
        max_shift: u32,
    ) -> Result<Option<diff::InferredMove>> {
        if !self.prev_valid || region.w == 0 || region.h == 0 || max_shift == 0 {
            return Ok(None);
        }
        if self.staging_cur.is_none() {
            self.staging_cur = Some(create_texture(
                device,
                self.width,
                self.height,
                Usage::Read,
            )?);
        }
        if self.staging_prev.is_none() {
            self.staging_prev = Some(create_texture(
                device,
                self.width,
                self.height,
                Usage::Read,
            )?);
        }
        let prev = self.prev.as_ref().ok_or("previous-frame texture missing")?;
        let staging_cur = self
            .staging_cur
            .as_ref()
            .ok_or("current staging texture missing")?;
        let staging_prev = self
            .staging_prev
            .as_ref()
            .ok_or("previous staging texture missing")?;
        let box_ = D3D11_BOX {
            left: region.x,
            top: region.y,
            front: 0,
            right: region.x + region.w,
            bottom: region.y + region.h,
            back: 1,
        };
        // SAFETY: the region is clipped to all four equal-sized BGRA textures.
        unsafe {
            context.CopySubresourceRegion(
                staging_cur,
                0,
                region.x,
                region.y,
                0,
                texture,
                0,
                Some(&box_ as *const D3D11_BOX),
            );
            context.CopySubresourceRegion(
                staging_prev,
                0,
                region.x,
                region.y,
                0,
                prev,
                0,
                Some(&box_ as *const D3D11_BOX),
            );
        }

        let mut cur_map = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: live staging subresource 0, mapped read-only exactly once.
        unsafe { context.Map(staging_cur, 0, D3D11_MAP_READ, 0, Some(&mut cur_map)) }?;
        let mut prev_map = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: as above, for the other staging texture.
        if let Err(error) =
            unsafe { context.Map(staging_prev, 0, D3D11_MAP_READ, 0, Some(&mut prev_map)) }
        {
            // SAFETY: balances the successful current map before returning.
            unsafe { context.Unmap(staging_cur, 0) };
            return Err(error.into());
        }

        let cur_pitch = cur_map.RowPitch as usize;
        let prev_pitch = prev_map.RowPitch as usize;
        // SAFETY: each mapping covers `height` rows at its reported row pitch.
        let cur = unsafe {
            std::slice::from_raw_parts(cur_map.pData as *const u8, cur_pitch * self.height as usize)
        };
        // SAFETY: as above, for the previous-frame staging map.
        let prev_pixels = unsafe {
            std::slice::from_raw_parts(
                prev_map.pData as *const u8,
                prev_pitch * self.height as usize,
            )
        };
        let inferred = diff::infer_vertical_move(
            cur,
            cur_pitch,
            prev_pixels,
            prev_pitch,
            self.width,
            self.height,
            damage,
            max_shift,
        );
        // SAFETY: exactly one unmap for each successful map above.
        unsafe {
            context.Unmap(staging_prev, 0);
            context.Unmap(staging_cur, 0);
        }
        Ok(inferred)
    }
}

/// Which of the two texture roles [`create_texture`] is making.
enum Usage {
    /// GPU-resident copy target, never mapped: the retained previous frame.
    Default,
    /// CPU-readable staging, mapped for the compare and the packing.
    Read,
}

/// One desktop-sized BGRA8 texture in the given role.
///
/// The format must match the frames exactly or `CopyResource` and
/// `CopySubresourceRegion` both refuse the copy, and both sources hand over BGRA8.
fn create_texture(
    device: &ID3D11Device,
    width: u32,
    height: u32,
    usage: Usage,
) -> Result<ID3D11Texture2D> {
    let (d3d_usage, cpu_access) = match usage {
        Usage::Default => (D3D11_USAGE_DEFAULT, 0),
        Usage::Read => (D3D11_USAGE_STAGING, D3D11_CPU_ACCESS_READ.0 as u32),
    };
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: d3d_usage,
        // Neither surface is ever bound to a pipeline stage: one is a copy
        // destination the CPU maps, the other a copy destination the GPU only ever
        // copies out of again.
        BindFlags: 0,
        CPUAccessFlags: cpu_access,
        MiscFlags: 0,
    };
    let mut texture: Option<ID3D11Texture2D> = None;
    // SAFETY: `desc` is fully initialised; the initial-data pointer is None because
    // the surface is filled by a copy, not by us.
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }?;
    texture.ok_or_else(|| "CreateTexture2D returned no texture".to_owned().into())
}
