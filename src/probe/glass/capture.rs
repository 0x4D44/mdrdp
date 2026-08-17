//! ScreenCaptureKit capture of one small screen region.
//!
//! The region is captured at its native pixel size and nothing else, so a frame is a few
//! kilobytes and diffing one costs nothing. Frames are delivered on ScreenCaptureKit's
//! own dispatch queue and handed to the measurement loop through an `mpsc` channel; the
//! measurement loop itself stays a plain synchronous read.
//!
//! **Every frame carries its presentation timestamp, not its arrival time.** The PTS is
//! the window server's own statement of when the frame was displayed, on the CoreMedia
//! host clock. Arrival time would add this process's scheduling delay to every sample.

use std::sync::Mutex;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::Duration;

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, DefinedClass, Message, define_class};
use objc2_core_foundation::CGRect;
use objc2_core_graphics::{CGDisplayCopyDisplayMode, CGDisplayMode};
use objc2_core_media::{CMSampleBuffer, CMTime, CMTimeFlags};
use objc2_core_video::{
    CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
    CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
    CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSNumber, NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCFrameStatus, SCShareableContent, SCStream, SCStreamConfiguration,
    SCStreamFrameInfoStatus, SCStreamOutput, SCStreamOutputType,
};

use super::Region;

/// `kCVPixelFormatType_32BGRA`, spelled the way CoreVideo spells it: a FourCC.
const PIXEL_FORMAT_BGRA: u32 = u32::from_be_bytes(*b"BGRA");

/// Everything ScreenCaptureKit does through a completion handler is waited on with this
/// bound, so a wedged capture service fails loudly instead of hanging the run.
const COMPLETION_TIMEOUT: Duration = Duration::from_secs(10);

/// A display that reports no refresh rate is assumed to run at this. Built-in Apple
/// displays return 0.0 from `CGDisplayModeGetRefreshRate`, so the fallback is the common
/// case on a laptop, not an edge case — which is why it is reported, not hidden.
pub const FALLBACK_REFRESH_HZ: f64 = 60.0;

/// One captured frame of the watched region, already flattened to tight BGRA rows.
#[derive(Debug, Clone)]
pub struct Frame {
    /// Presentation timestamp, microseconds on the CoreMedia host clock.
    pub pts_us: u64,
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

/// What the stream delivered at one instant.
#[derive(Debug, Clone)]
pub enum Sample {
    /// New pixels.
    Frame(Frame),
    /// The window server had nothing new to show at this timestamp — an `Idle`, `Blank`
    /// or `Suspended` status, or a sample with no pixel buffer. This is evidence of *no
    /// change* at a known time, which is exactly what the quiet and settle windows need.
    Idle { pts_us: u64 },
}

#[derive(Debug)]
pub struct Error(String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

fn err(msg: impl Into<String>) -> Error {
    Error(msg.into())
}

/// Convert a CoreMedia timestamp to microseconds. `None` for an invalid or indefinite
/// time — those must never be silently read as zero.
pub fn cmtime_to_us(t: CMTime) -> Option<u64> {
    if !t.flags.contains(CMTimeFlags::Valid) || t.timescale <= 0 {
        return None;
    }
    let micros = (t.value as i128) * 1_000_000 / (t.timescale as i128);
    u64::try_from(micros).ok()
}

/// A raw Objective-C object pointer moved between threads.
///
/// ScreenCaptureKit calls its completion handlers on an internal queue, so the retained
/// object has to cross a channel to the waiting thread. `Retained<T>` is not `Send` for
/// classes with thread affinity; these two (`SCShareableContent`, `SCDisplay`) have none,
/// and Objective-C reference counts are themselves thread-safe.
struct SendPtr<T>(*mut T);

// SAFETY: only ever constructed from `Retained::into_raw` for a class with no thread
// affinity, and consumed exactly once by `Retained::from_raw` on the receiving thread.
unsafe impl<T> Send for SendPtr<T> {}

/// The display the watched region lives on, plus the geometry the capture is built from.
pub struct DisplayTarget {
    pub display: Retained<SCDisplay>,
    pub id: u32,
    /// The display's frame in global points.
    pub frame: CGRect,
    /// Pixels per point — 2.0 on a Retina panel at its native scaling.
    pub scale: f64,
    pub refresh_hz: f64,
    /// Whether `refresh_hz` came from the display mode or from the fallback.
    pub refresh_from_mode: bool,
    pub pixel_width: usize,
    pub pixel_height: usize,
}

/// Find the display containing `region`, and read its mode.
///
/// This is also the first call that touches ScreenCaptureKit, so a missing screen
/// recording grant surfaces here — which is why the caller preflights first.
pub fn find_display(region: Region) -> Result<DisplayTarget, Error> {
    let content = shareable_content()?;
    let displays = unsafe { content.displays() };

    let mut chosen: Option<Retained<SCDisplay>> = None;
    for display in displays.iter() {
        let frame = unsafe { display.frame() };
        if region.is_inside(frame) {
            chosen = Some(display);
            break;
        }
    }
    let display = chosen.ok_or_else(|| {
        let bounds: Vec<String> = displays
            .iter()
            .map(|d| {
                let f = unsafe { d.frame() };
                format!(
                    "[{},{} {}x{}]",
                    f.origin.x, f.origin.y, f.size.width, f.size.height
                )
            })
            .collect();
        err(format!(
            "no display contains the region {region}; displays are {}",
            bounds.join(" ")
        ))
    })?;

    let id = unsafe { display.displayID() };
    let frame = unsafe { display.frame() };

    let mode = CGDisplayCopyDisplayMode(id)
        .ok_or_else(|| err(format!("display {id} reports no current display mode")))?;
    let pixel_width = CGDisplayMode::pixel_width(Some(&mode));
    let point_width = CGDisplayMode::width(Some(&mode));
    let scale = if point_width == 0 {
        1.0
    } else {
        pixel_width as f64 / point_width as f64
    };
    let raw_refresh = CGDisplayMode::refresh_rate(Some(&mode));
    let refresh_from_mode = raw_refresh > 0.0;
    let refresh_hz = if refresh_from_mode {
        raw_refresh
    } else {
        FALLBACK_REFRESH_HZ
    };

    Ok(DisplayTarget {
        display,
        id,
        frame,
        scale,
        refresh_hz,
        refresh_from_mode,
        pixel_width,
        pixel_height: (frame.size.height * scale).round() as usize,
    })
}

fn shareable_content() -> Result<Retained<SCShareableContent>, Error> {
    let (tx, rx) = channel::<Result<SendPtr<SCShareableContent>, String>>();
    let handler = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            // SAFETY: ScreenCaptureKit hands back either a live object or null.
            let message = unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string());
            let sent = match (unsafe { content.as_ref() }, message) {
                (Some(c), _) => Ok(SendPtr(Retained::into_raw(c.retain()))),
                (None, Some(m)) => Err(m),
                (None, None) => Err("no shareable content and no error".to_owned()),
            };
            let _ = tx.send(sent);
        },
    );

    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler) };

    match rx.recv_timeout(COMPLETION_TIMEOUT) {
        // SAFETY: the pointer came from `Retained::into_raw` in the handler above and is
        // consumed exactly once here.
        Ok(Ok(ptr)) => unsafe { Retained::from_raw(ptr.0) }
            .ok_or_else(|| err("shareable content pointer was null")),
        Ok(Err(message)) => Err(err(format!(
            "ScreenCaptureKit refused to enumerate displays: {message}. \
             This is what a missing Screen Recording grant looks like."
        ))),
        Err(_) => Err(err(
            "ScreenCaptureKit did not answer getShareableContent within 10 s",
        )),
    }
}

/// Instance state of the frame sink. The callback runs on ScreenCaptureKit's serial
/// queue, but `&self` is shared, so the sender needs a lock to be sound.
struct SinkIvars {
    tx: Mutex<Sender<Sample>>,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - FrameSink does not implement Drop.
    #[unsafe(super(NSObject))]
    #[name = "MdrdpGlassFrameSink"]
    #[ivars = SinkIvars]
    struct FrameSink;

    unsafe impl NSObjectProtocol for FrameSink {}

    unsafe impl SCStreamOutput for FrameSink {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn stream_did_output(
            &self,
            _stream: &SCStream,
            sample: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            if kind != SCStreamOutputType::Screen {
                return;
            }
            if let Some(delivered) = extract_sample(sample)
                && let Ok(tx) = self.ivars().tx.lock()
            {
                let _ = tx.send(delivered);
            }
        }
    }
);

impl FrameSink {
    fn new(tx: Sender<Sample>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(SinkIvars { tx: Mutex::new(tx) });
        unsafe { objc2::msg_send![super(this), init] }
    }
}

/// Whether ScreenCaptureKit says this sample carries a genuinely new frame.
///
/// The stream also delivers `Idle`, `Blank` and `Suspended` samples to keep the pipeline
/// alive. Treating one of those as a frame would either fabricate a change or, worse,
/// reset the settle window forever.
fn frame_is_complete(sample: &CMSampleBuffer) -> bool {
    let Some(attachments) = (unsafe { sample.sample_attachments_array(false) }) else {
        // No attachments at all: older behaviour, and a sample with a pixel buffer is
        // still a frame. Let the pixel-buffer check downstream decide.
        return true;
    };
    // CFArray of CFDictionary is toll-free bridged to NSArray of NSDictionary.
    // SAFETY: the bridge is guaranteed by CoreFoundation; the array is kept alive by
    // `attachments` for the whole borrow.
    let array: &NSArray<NSDictionary<objc2_foundation::NSString, NSNumber>> =
        unsafe { &*(&*attachments as *const _ as *const NSArray<_>) };
    let Some(dict) = array.firstObject() else {
        return true;
    };
    match unsafe { dict.objectForKey(SCStreamFrameInfoStatus) } {
        Some(status) => status.integerValue() == SCFrameStatus::Complete.0,
        None => true,
    }
}

/// Turn one delivered sample into either new pixels or a no-change tick.
///
/// Returns `None` only when the sample carries no usable timestamp — there is nothing to
/// say about a moment we cannot place on the clock. The callback must never panic,
/// because unwinding out of an Objective-C frame is undefined behaviour.
fn extract_sample(sample: &CMSampleBuffer) -> Option<Sample> {
    let pts_us = cmtime_to_us(unsafe { sample.presentation_time_stamp() })?;
    if !frame_is_complete(sample) {
        return Some(Sample::Idle { pts_us });
    }
    let Some(buffer) = (unsafe { sample.image_buffer() }) else {
        return Some(Sample::Idle { pts_us });
    };

    // SAFETY: the pixel buffer is alive for the whole of this block, and every read
    // below is bounded by the width/height/stride the buffer itself reports.
    unsafe {
        if CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags::ReadOnly) != 0 {
            return Some(Sample::Idle { pts_us });
        }
        let base = CVPixelBufferGetBaseAddress(&buffer).cast::<u8>();
        let width = CVPixelBufferGetWidth(&buffer);
        let height = CVPixelBufferGetHeight(&buffer);
        let stride = CVPixelBufferGetBytesPerRow(&buffer);

        let delivered = if base.is_null() || width == 0 || height == 0 || stride < width * 4 {
            Sample::Idle { pts_us }
        } else {
            let row_bytes = width * 4;
            let mut pixels = Vec::with_capacity(row_bytes * height);
            for row in 0..height {
                let src = base.add(row * stride);
                pixels.extend_from_slice(std::slice::from_raw_parts(src, row_bytes));
            }
            Sample::Frame(Frame {
                pts_us,
                width,
                height,
                pixels,
            })
        };
        CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags::ReadOnly);
        Some(delivered)
    }
}

/// A running capture of one region.
pub struct Capture {
    stream: Retained<SCStream>,
    output: Retained<ProtocolObject<dyn SCStreamOutput>>,
    rx: Receiver<Sample>,
    /// Held for the lifetime of the stream: ScreenCaptureKit does not retain the queue.
    _queue: DispatchRetained<DispatchQueue>,
    /// The destination size the stream was configured with, in pixels.
    pub width: usize,
    pub height: usize,
    /// The frame interval the stream was configured with, in microseconds.
    pub interval_us: u64,
}

impl Capture {
    /// Start capturing `region` (global points) from `target` at `fps` frames a second.
    pub fn start(target: &DisplayTarget, region: Region, fps: f64) -> Result<Self, Error> {
        let width = (region.w * target.scale).round() as usize;
        let height = (region.h * target.scale).round() as usize;
        if width == 0 || height == 0 {
            return Err(err(format!(
                "region {region} is smaller than one pixel at scale {}",
                target.scale
            )));
        }

        let config = unsafe { SCStreamConfiguration::new() };
        unsafe {
            config.setWidth(width);
            config.setHeight(height);
            // sourceRect is relative to the display's own origin, not the global space.
            config.setSourceRect(region.relative_to(target.frame));
            config.setPixelFormat(PIXEL_FORMAT_BGRA);
            // A moving cursor over the watched cell would read as a photon.
            config.setShowsCursor(false);
            config.setQueueDepth(8);
            config.setMinimumFrameInterval(CMTime {
                value: 1_000_000,
                timescale: (fps * 1_000_000.0).round() as i32,
                flags: CMTimeFlags::Valid,
                epoch: 0,
            });
        }

        let empty = NSArray::new();
        let filter = unsafe {
            SCContentFilter::initWithDisplay_excludingWindows(
                SCContentFilter::alloc(),
                &target.display,
                &empty,
            )
        };

        let (tx, rx) = channel::<Sample>();
        let sink = FrameSink::new(tx);
        let output = ProtocolObject::from_retained(sink);

        let stream = unsafe {
            SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &filter,
                &config,
                None,
            )
        };
        let queue = DispatchQueue::new("com.mdrdp.probe.glass", DispatchQueueAttr::SERIAL);
        unsafe {
            stream.addStreamOutput_type_sampleHandlerQueue_error(
                &output,
                SCStreamOutputType::Screen,
                Some(&queue),
            )
        }
        .map_err(|e| {
            err(format!(
                "addStreamOutput failed: {}",
                e.localizedDescription()
            ))
        })?;

        wait_for_completion("startCapture", |handler| unsafe {
            stream.startCaptureWithCompletionHandler(Some(handler))
        })?;

        Ok(Self {
            stream,
            output,
            rx,
            _queue: queue,
            width,
            height,
            interval_us: (1_000_000.0 / fps).round() as u64,
        })
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<Sample, RecvTimeoutError> {
        self.rx.recv_timeout(timeout)
    }

    /// Empty the queue, returning the most recent complete frame in it.
    ///
    /// Draining stops a trial from starting against a backlog; returning the last frame
    /// stops it from throwing away the only reference a still region will ever give it.
    pub fn drain(&self) -> Option<Frame> {
        let mut latest = None;
        while let Ok(sample) = self.rx.try_recv() {
            if let Sample::Frame(frame) = sample {
                latest = Some(frame);
            }
        }
        latest
    }

    pub fn stop(self) -> Result<(), Error> {
        let result = wait_for_completion("stopCapture", |handler| unsafe {
            self.stream.stopCaptureWithCompletionHandler(Some(handler))
        });
        let _ = unsafe {
            self.stream
                .removeStreamOutput_type_error(&self.output, SCStreamOutputType::Screen)
        };
        result
    }
}

/// Drive one of ScreenCaptureKit's `…WithCompletionHandler:` calls synchronously.
fn wait_for_completion<F>(what: &str, call: F) -> Result<(), Error>
where
    F: FnOnce(&block2::DynBlock<dyn Fn(*mut NSError)>),
{
    let (tx, rx) = channel::<Option<String>>();
    let handler = RcBlock::new(move |error: *mut NSError| {
        // SAFETY: the handler is called with either a live NSError or null.
        let message = unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string());
        let _ = tx.send(message);
    });
    call(&handler);

    match rx.recv_timeout(COMPLETION_TIMEOUT) {
        Ok(None) => Ok(()),
        Ok(Some(message)) => Err(err(format!("{what} failed: {message}"))),
        Err(_) => Err(err(format!("{what} did not complete within 10 s"))),
    }
}
