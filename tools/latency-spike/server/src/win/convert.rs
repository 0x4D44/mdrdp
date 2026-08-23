//! Stage 2 — BGRA→NV12 on the GPU, via the D3D11 video processor.
//!
//! The contract:
//!
//! 1. `ID3D11Device` → `ID3D11VideoDevice`, `ID3D11DeviceContext` →
//!    `ID3D11VideoContext` (both are QueryInterface, not new objects, so the encoder
//!    and the capture share one device).
//! 2. `CreateVideoProcessorEnumerator` from a content desc that fixes the input and
//!    output geometry, then `CreateVideoProcessor(enum, 0)`.
//! 3. Colour spaces on both sides — see `crate::colorspace` for the bit layout, which
//!    is the part that silently produces a washed-out picture when it is wrong.
//! 4. `OUTPUT_RATE_NORMAL` with no repeat and no custom rate: any frame-rate
//!    conversion here would add a frame of latency for nothing.
//! 5. Per frame: `CreateVideoProcessorInputView` on the captured texture (a new
//!    texture each frame, so the view cannot be cached), then `VideoProcessorBlt`
//!    into an NV12 texture that no encoder submission still owns.
//!
//! The pool exists because the encoder may still be reading frame *n-1* when frame
//! *n* is converted. A surface remains leased until the matching encoder output is
//! returned. The budget is fixed: the pipeline pumps the encoder before conversion
//! and sheds a whole desktop frame if any tile still has no free surface.
//!
//! **`VideoProcessorBlt` submits; it does not wait.** `convert_end_us` is when the
//! call returned, not when the GPU finished. The real cost shows up as back-pressure
//! inside the encode stage, and the README says so.

use super::Result;
use crate::colorspace;
use crate::surface_pool::LeaseSlots;
use std::mem::ManuallyDrop;
use windows::core::Interface;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, ID3D11VideoContext, ID3D11VideoDevice,
    ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorOutputView,
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
    D3D11_VIDEO_PROCESSOR_COLOR_SPACE, D3D11_VIDEO_PROCESSOR_CONTENT_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0,
    D3D11_VIDEO_PROCESSOR_OUTPUT_RATE_NORMAL, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
    D3D11_VIDEO_USAGE_PLAYBACK_NORMAL, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC};

/// Maximum NV12 surfaces per tile. Four allows the GPU and encoder to overlap while
/// bounding 5K surface memory to roughly 88 MiB across the normal two-tile layout.
pub const POOL_SIZE: usize = 4;

struct Surface {
    texture: ID3D11Texture2D,
    output_view: ID3D11VideoProcessorOutputView,
}

pub struct ConvertedSurface {
    pub texture: ID3D11Texture2D,
    pub slot: usize,
}

pub struct Nv12Converter {
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    surfaces: Vec<Surface>,
    leases: LeaseSlots,
}

impl Nv12Converter {
    pub fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        width: u32,
        height: u32,
        fps: u32,
    ) -> Result<Self> {
        Self::new_region(device, context, width, height, 0, 0, width, height, fps)
    }

    /// Convert one fixed source rectangle into a same-sized NV12 output. A 5K
    /// desktop owns two instances, one for each 2560-pixel-wide vertical tile.
    #[allow(clippy::too_many_arguments)]
    pub fn new_region(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        source_width: u32,
        source_height: u32,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        fps: u32,
    ) -> Result<Self> {
        let video_device: ID3D11VideoDevice = device.cast()?;
        let video_context: ID3D11VideoContext = context.cast()?;

        let rate = DXGI_RATIONAL {
            Numerator: fps,
            Denominator: 1,
        };
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: source_width,
            InputHeight: source_height,
            OutputFrameRate: rate,
            OutputWidth: width,
            OutputHeight: height,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        // SAFETY: `content` is a fully initialised local, live for the call.
        let enumerator = unsafe { video_device.CreateVideoProcessorEnumerator(&content) }?;
        // SAFETY: `enumerator` is live; rate-conversion index 0 always exists.
        let processor = unsafe { video_device.CreateVideoProcessor(&enumerator, 0) }?;

        let input_cs = D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
            _bitfield: colorspace::desktop_bgra_input(),
        };
        let output_cs = D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
            _bitfield: colorspace::nv12_output(),
        };
        // SAFETY: `processor` is live and every pointer argument is a live local.
        unsafe {
            video_context.VideoProcessorSetStreamColorSpace(&processor, 0, &input_cs);
            video_context.VideoProcessorSetOutputColorSpace(&processor, &output_cs);
            video_context.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            // No frame-rate conversion and no frame repetition: either would cost a
            // frame of latency and buy nothing for a screen-share workload.
            video_context.VideoProcessorSetStreamOutputRate(
                &processor,
                0,
                D3D11_VIDEO_PROCESSOR_OUTPUT_RATE_NORMAL,
                false,
                None,
            );
            let source_rect = RECT {
                left: i32::try_from(x)?,
                top: i32::try_from(y)?,
                right: i32::try_from(x.saturating_add(width))?,
                bottom: i32::try_from(y.saturating_add(height))?,
            };
            video_context.VideoProcessorSetStreamSourceRect(
                &processor,
                0,
                true,
                Some(&source_rect),
            );
            video_context.VideoProcessorSetStreamDestRect(&processor, 0, false, None);
            video_context.VideoProcessorSetOutputTargetRect(&processor, false, None);
        }

        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            // RENDER_TARGET is what a video-processor output view requires.
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut surfaces = Vec::with_capacity(POOL_SIZE);
        for _ in 0..POOL_SIZE {
            surfaces.push(Self::create_surface(
                device,
                &video_device,
                &enumerator,
                &desc,
            )?);
        }

        Ok(Self {
            video_device,
            video_context,
            enumerator,
            processor,
            surfaces,
            leases: LeaseSlots::new(POOL_SIZE),
        })
    }

    fn create_surface(
        device: &ID3D11Device,
        video_device: &ID3D11VideoDevice,
        enumerator: &ID3D11VideoProcessorEnumerator,
        desc: &D3D11_TEXTURE2D_DESC,
    ) -> Result<Surface> {
        let mut texture: Option<ID3D11Texture2D> = None;
        // SAFETY: `desc` is fully initialised; the initial-data pointer is None
        // because the surface is written by the video processor, not by us.
        unsafe { device.CreateTexture2D(desc, None, Some(&mut texture)) }?;
        let texture = texture.ok_or("CreateTexture2D returned no NV12 texture")?;

        let view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let mut output_view = None;
        // SAFETY: texture, enumerator and desc are all live for the call.
        unsafe {
            video_device.CreateVideoProcessorOutputView(
                &texture,
                enumerator,
                &view_desc,
                Some(&mut output_view),
            )
        }?;
        Ok(Surface {
            texture,
            output_view: output_view.ok_or("CreateVideoProcessorOutputView returned nothing")?,
        })
    }

    pub fn has_capacity(&self) -> bool {
        self.leases.available()
    }

    /// Convert one captured BGRA texture into a leased NV12 surface.
    pub fn convert(&mut self, source: &ID3D11Texture2D) -> Result<ConvertedSurface> {
        // The captured texture is a different object every frame, so its input view
        // cannot be cached the way the output views are.
        let view_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV {
                    MipSlice: 0,
                    ArraySlice: 0,
                },
            },
        };
        let mut input_view = None;
        // SAFETY: source, enumerator and desc are live for the call.
        unsafe {
            self.video_device.CreateVideoProcessorInputView(
                source,
                &self.enumerator,
                &view_desc,
                Some(&mut input_view),
            )
        }?;

        let slot = self
            .leases
            .acquire()
            .ok_or("NV12 surface budget exhausted")?;

        let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            ..Default::default()
        };
        // The struct holds its interface pointers in `ManuallyDrop`, so ownership of
        // `input_view` moves in here and must be reclaimed below — exactly once,
        // whether the blit succeeded or not.
        stream.pInputSurface = ManuallyDrop::new(input_view);

        // SAFETY: processor, output view and the one-element stream slice are all
        // live for the call; `stream` is fully initialised by `Default` plus the two
        // fields set above.
        let blt = unsafe {
            self.video_context.VideoProcessorBlt(
                &self.processor,
                &self.surfaces[slot].output_view,
                0,
                std::slice::from_ref(&stream),
            )
        };
        // Reclaim the reference handed to the struct. `D3D11_VIDEO_PROCESSOR_STREAM`
        // has no `Drop`, so without this the input view would leak every frame.
        drop(ManuallyDrop::into_inner(stream.pInputSurface));
        if let Err(error) = blt {
            self.leases.release(slot)?;
            return Err(error.into());
        }

        Ok(ConvertedSurface {
            texture: self.surfaces[slot].texture.clone(),
            slot,
        })
    }

    /// Return a surface after the encoder emitted or retired its submission.
    pub fn release(&mut self, slot: usize) -> Result<()> {
        self.leases.release(slot).map_err(Into::into)
    }
}
