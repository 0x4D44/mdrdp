//! Opt-in capture for the MS-RDPEAI AUDIO_INPUT dynamic virtual channel.
//!
//! The session thread handles protocol messages and queues capture commands. A worker
//! owns the cpal stream, converts samples, and packetizes audio. The cpal callback only
//! places samples on a bounded queue and never waits for the session or worker.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ironrdp::core::{Encode, EncodeResult, WriteCursor, ensure_size, impl_as_any};
use ironrdp::pdu::PduResult;
use ironrdp_dvc::{DvcChannelListener, DvcEncode, DvcMessage, DvcProcessor, DynamicChannelId};
use tracing::error;

const MSG_VERSION: u8 = 0x01;
const MSG_FORMATS: u8 = 0x02;
const MSG_OPEN: u8 = 0x03;
const MSG_OPEN_REPLY: u8 = 0x04;
const MSG_DATA_INCOMING: u8 = 0x05;
const MSG_DATA: u8 = 0x06;
const MSG_FORMAT_CHANGE: u8 = 0x07;
const CLIENT_VERSION: u32 = 2;

const MAX_FORMATS: usize = 64;
const MAX_PDU_BYTES: usize = 16 * 1024;
const MAX_PACKET_BYTES: u32 = 32 * 1024;
const RAW_QUEUE_CHUNKS: usize = 8;
const OUTBOUND_QUEUE_PACKETS: usize = 8;
const COMMAND_QUEUE_CAPACITY: usize = 8;
/// Windows servers terminate AUDIO_INPUT if OPEN_REPLY takes five seconds.
const OPEN_REPLY_TIMEOUT: Duration = Duration::from_secs(3);
const HRESULT_E_FAIL: u32 = 0x8000_4005;
const HRESULT_E_INVALIDARG: u32 = 0x8007_0057;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PcmFormat {
    channels: u16,
    sample_rate: u32,
}

fn u16_at(bytes: &[u8], offset: usize) -> Option<u16> {
    let end = offset.checked_add(2)?;
    Some(u16::from_le_bytes(bytes.get(offset..end)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    Some(u32::from_le_bytes(bytes.get(offset..end)?.try_into().ok()?))
}

/// Parse WAVEFORMATEX, retaining only PCM16 mono/stereo at the rates we support.
fn wave_at(bytes: &[u8], offset: usize) -> Option<(Option<PcmFormat>, usize)> {
    let tag = u16_at(bytes, offset)?;
    let channels = u16_at(bytes, offset.checked_add(2)?)?;
    let sample_rate = u32_at(bytes, offset.checked_add(4)?)?;
    let avg_bytes = u32_at(bytes, offset.checked_add(8)?)?;
    let block_align = u16_at(bytes, offset.checked_add(12)?)?;
    let bits = u16_at(bytes, offset.checked_add(14)?)?;
    let extra = usize::from(u16_at(bytes, offset.checked_add(16)?)?);
    let end = offset.checked_add(18)?.checked_add(extra)?;
    bytes.get(offset..end)?;

    let expected_align = channels.checked_mul(2);
    let expected_rate = expected_align.and_then(|align| sample_rate.checked_mul(u32::from(align)));
    let supported = tag == 1
        && (channels == 1 || channels == 2)
        && (sample_rate == 44_100 || sample_rate == 48_000)
        && bits == 16
        && extra == 0
        && expected_align == Some(block_align)
        && expected_rate == Some(avg_bytes);
    Some((
        supported.then_some(PcmFormat {
            channels,
            sample_rate,
        }),
        end,
    ))
}

/// Parse the server's bounded format list. Its packet-size field is reserved server-side.
fn parse_formats(pdu: &[u8]) -> Option<Vec<PcmFormat>> {
    if pdu.first().copied() != Some(MSG_FORMATS) || pdu.len() < 9 || pdu.len() > MAX_PDU_BYTES {
        return None;
    }
    let count = usize::try_from(u32_at(pdu, 1)?).ok()?;
    if count == 0 || count > MAX_FORMATS {
        return None;
    }
    let mut offset = 9;
    let mut formats = Vec::new();
    for _ in 0..count {
        let (format, end) = wave_at(pdu, offset)?;
        if let Some(format) = format
            && !formats.contains(&format)
        {
            formats.push(format);
        }
        offset = end;
    }
    Some(formats)
}

fn parse_open(pdu: &[u8], formats: &[PcmFormat]) -> Option<(usize, u32)> {
    if pdu.first().copied() != Some(MSG_OPEN) || pdu.len() > MAX_PDU_BYTES {
        return None;
    }
    let frames = u32_at(pdu, 1)?;
    let index = usize::try_from(u32_at(pdu, 5)?).ok()?;
    let (format, end) = wave_at(pdu, 9)?;
    let format = format?;
    let packet_bytes = frames.checked_mul(u32::from(format.channels) * 2)?;
    if formats.get(index).copied()? != format
        || frames == 0
        || packet_bytes > MAX_PACKET_BYTES
        || end != pdu.len()
    {
        return None;
    }
    Some((index, frames))
}

fn format_bytes(format: PcmFormat, out: &mut Vec<u8>) {
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&format.channels.to_le_bytes());
    out.extend_from_slice(&format.sample_rate.to_le_bytes());
    let block_align = format.channels * 2;
    out.extend_from_slice(&(format.sample_rate * u32::from(block_align)).to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
}

fn formats_pdu(formats: &[PcmFormat]) -> Vec<u8> {
    let packet_size = 9 + 18 * formats.len();
    let mut pdu = Vec::with_capacity(packet_size);
    pdu.push(MSG_FORMATS);
    pdu.extend_from_slice(&(formats.len() as u32).to_le_bytes());
    pdu.extend_from_slice(&(packet_size as u32).to_le_bytes());
    for format in formats {
        format_bytes(*format, &mut pdu);
    }
    pdu
}

fn open_reply(result: u32) -> Vec<u8> {
    let mut pdu = Vec::with_capacity(5);
    pdu.push(MSG_OPEN_REPLY);
    pdu.extend_from_slice(&result.to_le_bytes());
    pdu
}

fn format_change(index: usize) -> Vec<u8> {
    let mut pdu = Vec::with_capacity(5);
    pdu.push(MSG_FORMAT_CHANGE);
    pdu.extend_from_slice(&(index as u32).to_le_bytes());
    pdu
}

fn dvc_messages(pdus: impl IntoIterator<Item = Vec<u8>>) -> Vec<DvcMessage> {
    pdus.into_iter()
        .map(|pdu| Box::new(UnframedMicPdu(pdu)) as DvcMessage)
        .collect()
}

#[derive(Debug)]
struct UnframedMicPdu(Vec<u8>);

impl Encode for UnframedMicPdu {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: self.0.len());
        dst.write_slice(&self.0);
        Ok(())
    }

    fn name(&self) -> &'static str {
        "UnframedMicPdu"
    }

    fn size(&self) -> usize {
        self.0.len()
    }
}

impl DvcEncode for UnframedMicPdu {}

pub(crate) fn encode_dvc_messages(
    channel_id: u32,
    pdus: Vec<Vec<u8>>,
) -> ironrdp::core::EncodeResult<Vec<ironrdp_svc::SvcMessage>> {
    ironrdp_dvc::encode_dvc_messages(
        channel_id,
        dvc_messages(pdus),
        ironrdp_svc::ChannelFlags::SHOW_PROTOCOL,
    )
}

#[derive(Debug)]
enum CaptureCommand {
    Start {
        channel_id: u32,
        generation: u64,
        format: PcmFormat,
        frames: u32,
        format_index: usize,
        capture_cancel: Arc<AtomicBool>,
        open_reply: Option<Arc<OpenReplyTicket>>,
    },
    Shutdown,
}

#[derive(Debug)]
struct OpenReplyTicket {
    state: Mutex<OpenReplyState>,
    capture_cancel: Arc<AtomicBool>,
}

#[derive(Debug)]
struct OpenReplyState {
    deadline: Instant,
    completed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenReplyCompletion {
    OnTime,
    TimedOut,
    AlreadyCompleted,
}

impl OpenReplyTicket {
    fn new(now: Instant, capture_cancel: Arc<AtomicBool>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(OpenReplyState {
                deadline: now + OPEN_REPLY_TIMEOUT,
                completed: false,
            }),
            capture_cancel,
        })
    }

    fn complete(&self, now: Instant) -> OpenReplyCompletion {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.completed {
            return OpenReplyCompletion::AlreadyCompleted;
        }
        state.completed = true;
        if now >= state.deadline {
            self.capture_cancel.store(true, Ordering::Release);
            OpenReplyCompletion::TimedOut
        } else {
            OpenReplyCompletion::OnTime
        }
    }

    fn expire(&self, now: Instant) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.completed || now < state.deadline {
            return false;
        }
        state.completed = true;
        self.capture_cancel.store(true, Ordering::Release);
        true
    }

    fn cancel(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .completed = true;
        self.capture_cancel.store(true, Ordering::Release);
    }
}

/// A worker response tied to the DVC instance that requested it.
pub(crate) struct MicOutbound {
    pub(crate) channel_id: u32,
    pub(crate) generation: u64,
    pub(crate) pdus: Vec<Vec<u8>>,
}

#[derive(Default)]
struct OutboundQueue {
    entries: Mutex<VecDeque<MicOutbound>>,
}

impl OutboundQueue {
    fn push(&self, message: MicOutbound, control: bool) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if entries.len() >= OUTBOUND_QUEUE_PACKETS {
            if control {
                if let Some(index) = entries.iter().position(|entry| {
                    entry
                        .pdus
                        .first()
                        .is_some_and(|pdu| pdu.first() == Some(&MSG_DATA_INCOMING))
                }) {
                    entries.remove(index);
                } else {
                    entries.pop_front();
                }
            } else {
                entries.pop_front();
            }
        }
        entries.push_back(message);
    }

    fn pop(&self) -> Option<MicOutbound> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
    }

    fn remove_generation(&self, generation: u64) {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|entry| entry.generation != generation);
    }

    fn clear(&self) {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

struct CaptureCore {
    commands: SyncSender<CaptureCommand>,
    outputs: Arc<OutboundQueue>,
    next_generation: AtomicU64,
    stopped: Arc<AtomicBool>,
}

impl CaptureCore {
    fn shutdown(&self) {
        // Signal instead of joining: an OS privacy/device call may still be opening.
        if !self.stopped.swap(true, Ordering::AcqRel) {
            let _ = self.commands.try_send(CaptureCommand::Shutdown);
        }
        self.outputs.clear();
    }
}

/// Listener registered only when the microphone setting is enabled.
pub struct MicrophoneListener {
    core: Arc<CaptureCore>,
}

impl DvcChannelListener for MicrophoneListener {
    fn channel_name(&self) -> &str {
        "AUDIO_INPUT"
    }

    fn create(&mut self, _channel_id: DynamicChannelId) -> Option<Box<dyn DvcProcessor>> {
        let generation = self.core.next_generation.fetch_add(1, Ordering::Relaxed);
        Some(Box::new(MicrophoneDvc {
            commands: self.core.commands.clone(),
            outputs: Arc::clone(&self.core.outputs),
            stopped: Arc::clone(&self.core.stopped),
            generation,
            channel_id: None,
            formats: Vec::new(),
            frames_per_packet: None,
            capture_cancel: None,
            pending_open_reply: Mutex::new(None),
        }))
    }
}

/// MS-RDPEAI state for one server-created AUDIO_INPUT DVC.
pub struct MicrophoneDvc {
    commands: SyncSender<CaptureCommand>,
    outputs: Arc<OutboundQueue>,
    stopped: Arc<AtomicBool>,
    generation: u64,
    channel_id: Option<u32>,
    formats: Vec<PcmFormat>,
    frames_per_packet: Option<u32>,
    capture_cancel: Option<Arc<AtomicBool>>,
    pending_open_reply: Mutex<Option<Arc<OpenReplyTicket>>>,
}

impl_as_any!(MicrophoneDvc);

impl MicrophoneDvc {
    fn queue_start(
        &self,
        channel_id: u32,
        format_index: usize,
        frames: u32,
        capture_cancel: Arc<AtomicBool>,
        open_reply: Option<Arc<OpenReplyTicket>>,
    ) -> bool {
        let Some(format) = self.formats.get(format_index).copied() else {
            return false;
        };
        !self.stopped.load(Ordering::Acquire)
            && self
                .commands
                .try_send(CaptureCommand::Start {
                    channel_id,
                    generation: self.generation,
                    format,
                    frames,
                    format_index,
                    capture_cancel,
                    open_reply,
                })
                .is_ok()
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    fn cancel_capture(&mut self) {
        if let Some(cancel) = self.capture_cancel.take() {
            cancel.store(true, Ordering::Release);
        }
    }

    fn cancel_open_reply(&self) {
        if let Some(reply) = self
            .pending_open_reply
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            reply.cancel();
        }
    }

    pub(crate) fn expire_open_reply(&self, now: Instant) -> Option<MicOutbound> {
        let reply = self
            .pending_open_reply
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .cloned()?;
        if !reply.expire(now) {
            return None;
        }
        Some(MicOutbound {
            channel_id: self.channel_id?,
            generation: self.generation,
            pdus: vec![open_reply(HRESULT_E_FAIL)],
        })
    }
}

impl DvcProcessor for MicrophoneDvc {
    fn channel_name(&self) -> &str {
        "AUDIO_INPUT"
    }

    fn start(&mut self, channel_id: u32) -> PduResult<Vec<DvcMessage>> {
        self.cancel_capture();
        self.cancel_open_reply();
        self.channel_id = Some(channel_id);
        self.formats.clear();
        self.frames_per_packet = None;
        Ok(Vec::new())
    }

    fn process(&mut self, channel_id: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        if self.channel_id != Some(channel_id)
            || payload.is_empty()
            || payload.len() > MAX_PDU_BYTES
        {
            return Ok(Vec::new());
        }

        match payload[0] {
            MSG_VERSION if payload.len() == 5 => {
                let Some(server_version) = u32_at(payload, 1) else {
                    return Ok(Vec::new());
                };
                if (1..=CLIENT_VERSION).contains(&server_version) {
                    let mut response = Vec::with_capacity(5);
                    response.push(MSG_VERSION);
                    response.extend_from_slice(&CLIENT_VERSION.to_le_bytes());
                    Ok(dvc_messages([response]))
                } else {
                    Ok(Vec::new())
                }
            }
            MSG_FORMATS => {
                let Some(formats) = parse_formats(payload) else {
                    return Ok(Vec::new());
                };
                self.cancel_capture();
                self.cancel_open_reply();
                self.formats = formats;
                self.frames_per_packet = None;
                Ok(dvc_messages([
                    vec![MSG_DATA_INCOMING],
                    formats_pdu(&self.formats),
                ]))
            }
            MSG_OPEN => {
                let Some((format_index, frames)) = parse_open(payload, &self.formats) else {
                    self.cancel_capture();
                    self.cancel_open_reply();
                    self.frames_per_packet = None;
                    return Ok(dvc_messages([open_reply(HRESULT_E_INVALIDARG)]));
                };
                self.cancel_capture();
                self.cancel_open_reply();
                let capture_cancel = Arc::new(AtomicBool::new(false));
                let open_reply_ticket =
                    OpenReplyTicket::new(Instant::now(), Arc::clone(&capture_cancel));
                if self.queue_start(
                    channel_id,
                    format_index,
                    frames,
                    Arc::clone(&capture_cancel),
                    Some(Arc::clone(&open_reply_ticket)),
                ) {
                    self.capture_cancel = Some(capture_cancel);
                    *self
                        .pending_open_reply
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        Some(open_reply_ticket);
                    self.frames_per_packet = Some(frames);
                    // The worker replies after cpal opens the device. The session pump
                    // sends E_FAIL at the three-second deadline if device permission
                    // or startup is slow, so this callback never waits on the OS.
                    Ok(Vec::new())
                } else {
                    open_reply_ticket.cancel();
                    self.frames_per_packet = None;
                    Ok(dvc_messages([open_reply(HRESULT_E_FAIL)]))
                }
            }
            MSG_FORMAT_CHANGE if payload.len() == 5 => {
                let Some(index) = u32_at(payload, 1).and_then(|value| usize::try_from(value).ok())
                else {
                    return Ok(Vec::new());
                };
                let Some(frames) = self.frames_per_packet else {
                    return Ok(Vec::new());
                };
                if self.formats.get(index).is_none() {
                    return Ok(Vec::new());
                }
                self.cancel_capture();
                let capture_cancel = Arc::new(AtomicBool::new(false));
                if self.queue_start(channel_id, index, frames, capture_cancel.clone(), None) {
                    self.capture_cancel = Some(capture_cancel);
                } else {
                    capture_cancel.store(true, Ordering::Release);
                }
                Ok(Vec::new())
            }
            // These messages travel from client to server. Unknown or misplaced input is
            // ignored so malformed audio traffic cannot panic or wedge desktop service.
            _ => Ok(Vec::new()),
        }
    }

    fn close(&mut self, channel_id: u32) {
        if self.channel_id != Some(channel_id) {
            return;
        }
        self.channel_id = None;
        self.cancel_capture();
        self.cancel_open_reply();
        self.formats.clear();
        self.frames_per_packet = None;
        self.outputs.remove_generation(self.generation);
    }
}

/// Session-thread side of capture. It drains bounded, packetized DVC messages.
pub struct MicrophonePump {
    core: Arc<CaptureCore>,
}

impl MicrophonePump {
    pub(crate) fn try_recv(&self) -> Option<MicOutbound> {
        self.core.outputs.pop()
    }

    pub(crate) fn shutdown(&self) {
        self.core.shutdown();
    }
}

impl Drop for MicrophonePump {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Create the DVC listener and pump queue. No input device opens until a valid OPEN PDU.
pub fn create(
    bell: crate::wake::Doorbell,
) -> std::io::Result<(MicrophoneListener, MicrophonePump)> {
    let (commands, command_rx) = mpsc::sync_channel(COMMAND_QUEUE_CAPACITY);
    let outputs = Arc::new(OutboundQueue::default());
    let worker_outputs = Arc::clone(&outputs);
    let worker_bell = bell.clone();
    let stopped = Arc::new(AtomicBool::new(false));
    let worker_stopped = Arc::clone(&stopped);
    // Detaching keeps session shutdown independent of the OS audio stack. The worker
    // drops a stream on close and exits when `stopped` is set, after any active call returns.
    drop(
        std::thread::Builder::new()
            .name("mdrdp-microphone".to_owned())
            .spawn(move || {
                capture_worker(command_rx, worker_outputs, worker_bell, worker_stopped)
            })?,
    );
    let core = Arc::new(CaptureCore {
        commands,
        outputs,
        next_generation: AtomicU64::new(1),
        stopped,
    });
    Ok((
        MicrophoneListener {
            core: Arc::clone(&core),
        },
        MicrophonePump { core },
    ))
}

struct RawAudio {
    samples: Vec<f32>,
    channels: u16,
    sample_rate: u32,
}

struct ActiveCapture {
    _stream: cpal::Stream,
    raw: Receiver<RawAudio>,
    device_failed: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    packetizer: Packetizer,
}

struct Packetizer {
    channel_id: u32,
    generation: u64,
    format: PcmFormat,
    frames_per_packet: usize,
    phase: f64,
    pending: VecDeque<i16>,
}

impl Packetizer {
    fn new(channel_id: u32, generation: u64, format: PcmFormat, frames: u32) -> Self {
        Self {
            channel_id,
            generation,
            format,
            frames_per_packet: usize::try_from(frames).unwrap_or(0),
            phase: 0.0,
            pending: VecDeque::new(),
        }
    }

    fn push(&mut self, audio: RawAudio, outputs: &OutboundQueue, bell: &crate::wake::Doorbell) {
        if audio.channels == 0 || audio.sample_rate == 0 {
            return;
        }
        let channels = usize::from(self.format.channels);
        let mapped =
            crate::audio::remap_channels(&audio.samples, audio.channels, self.format.channels);
        let source_frames = mapped.len() / channels;
        if source_frames == 0 {
            return;
        }

        let step = f64::from(audio.sample_rate) / f64::from(self.format.sample_rate);
        let mut position = self.phase;
        while position < source_frames as f64 {
            let first = (position.floor() as usize).min(source_frames - 1);
            let second = (first + 1).min(source_frames - 1);
            let fraction = (position - first as f64) as f32;
            for channel in 0..channels {
                let a = mapped[first * channels + channel];
                let b = mapped[second * channels + channel];
                self.pending.push_back(to_i16(a + (b - a) * fraction));
            }
            position += step;
        }
        self.phase = (position - source_frames as f64).max(0.0);

        let packet_samples = self.frames_per_packet * channels;
        if packet_samples == 0 {
            return;
        }
        while self.pending.len() >= packet_samples {
            let mut data = Vec::with_capacity(1 + packet_samples * 2);
            data.push(MSG_DATA);
            for _ in 0..packet_samples {
                data.extend_from_slice(&self.pending.pop_front().unwrap_or_default().to_le_bytes());
            }
            outputs.push(
                MicOutbound {
                    channel_id: self.channel_id,
                    generation: self.generation,
                    pdus: vec![vec![MSG_DATA_INCOMING], data],
                },
                false,
            );
            bell.ring();
        }
    }
}

fn to_i16(sample: f32) -> i16 {
    if !sample.is_finite() {
        return 0;
    }
    if sample <= -1.0 {
        i16::MIN
    } else {
        (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16
    }
}

fn capture_worker(
    commands: Receiver<CaptureCommand>,
    outputs: Arc<OutboundQueue>,
    bell: crate::wake::Doorbell,
    stopped: Arc<AtomicBool>,
) {
    let mut active: Option<ActiveCapture> = None;
    loop {
        if stopped.load(Ordering::Acquire) {
            return;
        }
        loop {
            if stopped.load(Ordering::Acquire) {
                return;
            }
            match commands.try_recv() {
                Ok(command) => {
                    if !apply_command(command, &mut active, &outputs, &bell, &stopped) {
                        return;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        if active.as_ref().is_some_and(|capture| {
            capture.cancelled.load(Ordering::Acquire)
                || capture.device_failed.swap(false, Ordering::AcqRel)
        }) {
            active = None;
            continue;
        }

        if let Some(capture) = active.as_mut() {
            match capture.raw.recv_timeout(Duration::from_millis(5)) {
                Ok(audio) => capture.packetizer.push(audio, &outputs, &bell),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => active = None,
            }
        } else {
            match commands.recv_timeout(Duration::from_millis(50)) {
                Ok(command) => {
                    if !apply_command(command, &mut active, &outputs, &bell, &stopped) {
                        return;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    }
}

fn apply_command(
    command: CaptureCommand,
    active: &mut Option<ActiveCapture>,
    outputs: &OutboundQueue,
    bell: &crate::wake::Doorbell,
    stopped: &AtomicBool,
) -> bool {
    match command {
        CaptureCommand::Start {
            channel_id,
            generation,
            format,
            frames,
            format_index,
            capture_cancel,
            open_reply: ticket,
        } => {
            *active = None;
            if stopped.load(Ordering::Acquire) || capture_cancel.load(Ordering::Acquire) {
                return !stopped.load(Ordering::Acquire);
            }
            match start_capture(
                channel_id,
                generation,
                format,
                frames,
                Arc::clone(&capture_cancel),
            ) {
                Ok(capture) => {
                    if stopped.load(Ordering::Acquire) || capture_cancel.load(Ordering::Acquire) {
                        return !stopped.load(Ordering::Acquire);
                    }
                    let open_completion =
                        ticket.as_ref().map(|reply| reply.complete(Instant::now()));
                    if open_completion == Some(OpenReplyCompletion::AlreadyCompleted) {
                        return true;
                    }
                    if open_completion == Some(OpenReplyCompletion::TimedOut) {
                        outputs.push(
                            MicOutbound {
                                channel_id,
                                generation,
                                pdus: vec![open_reply(HRESULT_E_FAIL)],
                            },
                            true,
                        );
                        bell.ring();
                        return true;
                    }
                    *active = Some(capture);
                    let pdus = if ticket.is_some() {
                        vec![format_change(format_index), open_reply(0)]
                    } else {
                        vec![format_change(format_index)]
                    };
                    outputs.push(
                        MicOutbound {
                            channel_id,
                            generation,
                            pdus,
                        },
                        true,
                    );
                    bell.ring();
                }
                Err(reason) => {
                    error!(%reason, "microphone: could not open the default input device");
                    if let Some(ticket) = ticket
                        && ticket.complete(Instant::now()) != OpenReplyCompletion::AlreadyCompleted
                    {
                        outputs.push(
                            MicOutbound {
                                channel_id,
                                generation,
                                pdus: vec![open_reply(HRESULT_E_FAIL)],
                            },
                            true,
                        );
                        bell.ring();
                    }
                }
            }
        }
        CaptureCommand::Shutdown => return false,
    }
    true
}

fn start_capture(
    channel_id: u32,
    generation: u64,
    format: PcmFormat,
    frames: u32,
    cancelled: Arc<AtomicBool>,
) -> Result<ActiveCapture, String> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| "no default input device".to_owned())?;
    let (config, sample_rate, channels, sample_format) = pick_input_config(&device)?;
    let (raw_tx, raw) = mpsc::sync_channel(RAW_QUEUE_CHUNKS);
    let device_failed = Arc::new(AtomicBool::new(false));
    let stream = match sample_format {
        cpal::SampleFormat::F32 => {
            let failed = Arc::clone(&device_failed);
            device
                .build_input_stream(
                    &config,
                    move |data: &[f32], _| {
                        enqueue_samples(data.iter().copied(), sample_rate, channels, &raw_tx)
                    },
                    move |err| {
                        error!(%err, "microphone: input device stopped");
                        failed.store(true, Ordering::Release);
                    },
                    None,
                )
                .map_err(|error| error.to_string())?
        }
        cpal::SampleFormat::I16 => {
            let failed = Arc::clone(&device_failed);
            device
                .build_input_stream(
                    &config,
                    move |data: &[i16], _| {
                        enqueue_samples(
                            data.iter().map(|sample| f32::from(*sample) / 32768.0),
                            sample_rate,
                            channels,
                            &raw_tx,
                        )
                    },
                    move |err| {
                        error!(%err, "microphone: input device stopped");
                        failed.store(true, Ordering::Release);
                    },
                    None,
                )
                .map_err(|error| error.to_string())?
        }
        cpal::SampleFormat::U16 => {
            let failed = Arc::clone(&device_failed);
            device
                .build_input_stream(
                    &config,
                    move |data: &[u16], _| {
                        enqueue_samples(
                            data.iter()
                                .map(|sample| (f32::from(*sample) - 32768.0) / 32768.0),
                            sample_rate,
                            channels,
                            &raw_tx,
                        )
                    },
                    move |err| {
                        error!(%err, "microphone: input device stopped");
                        failed.store(true, Ordering::Release);
                    },
                    None,
                )
                .map_err(|error| error.to_string())?
        }
        other => return Err(format!("unsupported input sample type {other:?}")),
    };
    stream.play().map_err(|error| error.to_string())?;

    Ok(ActiveCapture {
        _stream: stream,
        raw,
        device_failed,
        cancelled,
        packetizer: Packetizer::new(channel_id, generation, format, frames),
    })
}

fn enqueue_samples(
    samples: impl Iterator<Item = f32>,
    sample_rate: u32,
    channels: u16,
    sender: &SyncSender<RawAudio>,
) {
    let mut normalized: Vec<f32> = samples
        .map(|sample| {
            if sample.is_finite() {
                sample.clamp(-1.0, 1.0)
            } else {
                0.0
            }
        })
        .collect();
    let frame_samples = usize::from(channels);
    normalized.truncate(normalized.len() / frame_samples * frame_samples);
    if normalized.is_empty() {
        return;
    }
    // A full callback queue drops this chunk; the audio callback never blocks.
    let _ = sender.try_send(RawAudio {
        samples: normalized,
        channels,
        sample_rate,
    });
}

fn pick_input_config(
    device: &cpal::Device,
) -> Result<(cpal::StreamConfig, u32, u16, cpal::SampleFormat), String> {
    if let Ok(default) = device.default_input_config() {
        let format = default.sample_format();
        let channels = default.channels();
        if (channels == 1 || channels == 2) && supported_sample_format(format) {
            return Ok((default.config(), default.sample_rate().0, channels, format));
        }
    }
    let candidate = device
        .supported_input_configs()
        .map_err(|error| error.to_string())?
        .filter(|range| {
            (range.channels() == 1 || range.channels() == 2)
                && supported_sample_format(range.sample_format())
        })
        .max_by_key(|range| (range.channels() == 2, range.max_sample_rate().0))
        .ok_or_else(|| "no mono or stereo float/integer input format".to_owned())?
        .with_max_sample_rate();
    Ok((
        candidate.config(),
        candidate.sample_rate().0,
        candidate.channels(),
        candidate.sample_format(),
    ))
}

fn supported_sample_format(format: cpal::SampleFormat) -> bool {
    matches!(
        format,
        cpal::SampleFormat::F32 | cpal::SampleFormat::I16 | cpal::SampleFormat::U16
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ironrdp::core::encode_vec;

    fn wave(channels: u16, sample_rate: u32) -> Vec<u8> {
        let mut wire = Vec::new();
        format_bytes(
            PcmFormat {
                channels,
                sample_rate,
            },
            &mut wire,
        );
        wire
    }

    fn formats_pdu_for(formats: &[Vec<u8>]) -> Vec<u8> {
        let mut pdu = vec![MSG_FORMATS];
        pdu.extend_from_slice(&(formats.len() as u32).to_le_bytes());
        pdu.extend_from_slice(&0u32.to_le_bytes());
        for format in formats {
            pdu.extend_from_slice(format);
        }
        pdu
    }

    fn open_pdu(index: u32, frames: u32, format: PcmFormat) -> Vec<u8> {
        let mut pdu = vec![MSG_OPEN];
        pdu.extend_from_slice(&frames.to_le_bytes());
        pdu.extend_from_slice(&index.to_le_bytes());
        format_bytes(format, &mut pdu);
        pdu
    }

    fn bytes(messages: Vec<DvcMessage>) -> Vec<Vec<u8>> {
        messages
            .into_iter()
            .map(|message| encode_vec(message.as_ref()).expect("encode PDU"))
            .collect()
    }

    fn channel() -> (MicrophoneDvc, Receiver<CaptureCommand>) {
        let (commands, command_rx) = mpsc::sync_channel(COMMAND_QUEUE_CAPACITY);
        (
            MicrophoneDvc {
                commands,
                outputs: Arc::new(OutboundQueue::default()),
                stopped: Arc::new(AtomicBool::new(false)),
                generation: 7,
                channel_id: None,
                formats: Vec::new(),
                frames_per_packet: None,
                capture_cancel: None,
                pending_open_reply: Mutex::new(None),
            },
            command_rx,
        )
    }

    #[test]
    fn accepts_pcm_formats_and_bounds_packet_frames() {
        let accepted = parse_formats(&formats_pdu_for(&[wave(1, 48_000), wave(2, 44_100)]))
            .expect("formats parsed");
        assert_eq!(accepted.len(), 2);
        assert_eq!(accepted[0].sample_rate, 48_000);
        assert_eq!(accepted[1].channels, 2);
        assert_eq!(
            parse_open(
                &open_pdu(
                    0,
                    960,
                    PcmFormat {
                        channels: 1,
                        sample_rate: 48_000
                    }
                ),
                &accepted,
            ),
            Some((0, 960))
        );
        assert_eq!(
            parse_open(
                &open_pdu(
                    0,
                    u32::MAX,
                    PcmFormat {
                        channels: 1,
                        sample_rate: 48_000
                    }
                ),
                &accepted,
            ),
            None
        );
    }

    #[test]
    fn version_reply_and_formats_intersection_follow_wire_order() {
        let (mut channel, _) = channel();
        channel.start(12).expect("DVC starts");
        let mut version = vec![MSG_VERSION];
        version.extend_from_slice(&2u32.to_le_bytes());
        assert_eq!(
            bytes(channel.process(12, &version).expect("version handled")),
            vec![vec![MSG_VERSION, 2, 0, 0, 0]]
        );

        let server = formats_pdu_for(&[wave(1, 48_000), wave(1, 16_000), wave(2, 44_100)]);
        let response = bytes(channel.process(12, &server).expect("formats handled"));
        assert_eq!(response.len(), 2);
        assert_eq!(response[0], vec![MSG_DATA_INCOMING]);
        assert_eq!(u32_at(&response[1], 1), Some(2));
        assert_eq!(u32_at(&response[1], 5), Some(45));
        assert_eq!(
            parse_formats(&response[1]),
            Some(vec![
                PcmFormat {
                    channels: 1,
                    sample_rate: 48_000
                },
                PcmFormat {
                    channels: 2,
                    sample_rate: 44_100
                },
            ])
        );
    }

    #[test]
    fn capture_is_requested_only_by_a_valid_open() {
        let (mut channel, commands) = channel();
        channel.start(12).expect("DVC starts");
        assert!(matches!(commands.try_recv(), Err(TryRecvError::Empty)));
        channel
            .process(12, &formats_pdu_for(&[wave(1, 48_000)]))
            .expect("formats handled");
        assert!(matches!(commands.try_recv(), Err(TryRecvError::Empty)));

        let format = PcmFormat {
            channels: 1,
            sample_rate: 48_000,
        };
        assert!(
            channel
                .process(12, &open_pdu(0, 960, format))
                .expect("open handled")
                .is_empty()
        );
        let Ok(CaptureCommand::Start {
            channel_id: 12,
            generation: 7,
            format: selected,
            frames: 960,
            format_index: 0,
            capture_cancel,
            open_reply: Some(_),
        }) = commands.try_recv()
        else {
            panic!("valid OPEN queues capture start");
        };
        assert_eq!(selected, format);

        let malformed = open_pdu(0, u32::MAX, format);
        assert_eq!(
            bytes(channel.process(12, &malformed).expect("bad open contained")),
            vec![open_reply(HRESULT_E_INVALIDARG)]
        );
        assert!(capture_cancel.load(Ordering::Acquire));
        assert!(matches!(commands.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn open_reply_deadline_fails_once_without_waiting_for_device_open() {
        let (mut channel, commands) = channel();
        channel.start(12).expect("DVC starts");
        channel
            .process(12, &formats_pdu_for(&[wave(1, 48_000)]))
            .expect("formats handled");
        let format = PcmFormat {
            channels: 1,
            sample_rate: 48_000,
        };
        channel
            .process(12, &open_pdu(0, 960, format))
            .expect("open queued");
        let Ok(CaptureCommand::Start {
            open_reply: Some(ticket),
            ..
        }) = commands.try_recv()
        else {
            panic!("OPEN has a reply ticket");
        };
        ticket
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .deadline = Instant::now() - Duration::from_millis(1);

        let timeout = channel
            .expire_open_reply(Instant::now())
            .expect("expired OPEN gets a failure reply");
        assert_eq!(timeout.pdus, vec![open_reply(HRESULT_E_FAIL)]);
        assert_eq!(
            ticket.complete(Instant::now()),
            OpenReplyCompletion::AlreadyCompleted,
            "a late device result cannot reply twice"
        );
        assert!(
            ticket.capture_cancel.load(Ordering::Acquire),
            "a device that opens after timeout is discarded"
        );
    }

    #[test]
    fn command_queue_is_bounded_and_open_failure_is_immediate() {
        let (mut channel, commands) = channel();
        channel.start(12).expect("DVC starts");
        channel
            .process(12, &formats_pdu_for(&[wave(1, 48_000)]))
            .expect("formats handled");
        let format = PcmFormat {
            channels: 1,
            sample_rate: 48_000,
        };

        for index in 0..=COMMAND_QUEUE_CAPACITY {
            let response = bytes(
                channel
                    .process(12, &open_pdu(0, 960, format))
                    .expect("OPEN processed without blocking"),
            );
            if index < COMMAND_QUEUE_CAPACITY {
                assert!(response.is_empty(), "room remains for command {index}");
            } else {
                assert_eq!(response, vec![open_reply(HRESULT_E_FAIL)]);
            }
        }

        let queued = std::iter::from_fn(|| commands.try_recv().ok()).count();
        assert_eq!(queued, COMMAND_QUEUE_CAPACITY);
    }

    #[test]
    fn malformed_and_misrouted_messages_are_ignored_safely() {
        let (mut channel, _) = channel();
        channel.start(12).expect("DVC starts");
        assert!(
            channel
                .process(12, &[MSG_FORMATS, 1])
                .expect("truncated ignored")
                .is_empty()
        );
        assert!(
            channel
                .process(12, &[0xff])
                .expect("unknown ignored")
                .is_empty()
        );
        assert!(
            channel
                .process(99, &[MSG_VERSION, 2, 0, 0, 0])
                .expect("wrong channel ignored")
                .is_empty()
        );
    }

    #[test]
    fn packetizer_emits_whole_pcm_frames_with_incoming_marker() {
        let (bell, _wake) = crate::wake::doorbell().expect("doorbell");
        let outputs = OutboundQueue::default();
        let mut packetizer = Packetizer::new(
            12,
            7,
            PcmFormat {
                channels: 1,
                sample_rate: 48_000,
            },
            4,
        );
        packetizer.push(
            RawAudio {
                samples: vec![0.0, 0.25, -0.25],
                channels: 1,
                sample_rate: 48_000,
            },
            &outputs,
            &bell,
        );
        assert!(outputs.pop().is_none());

        packetizer.push(
            RawAudio {
                samples: vec![0.5, -0.5],
                channels: 1,
                sample_rate: 48_000,
            },
            &outputs,
            &bell,
        );
        let packet = outputs.pop().expect("one complete packet");
        assert_eq!(packet.channel_id, 12);
        assert_eq!(packet.generation, 7);
        assert_eq!(packet.pdus[0], vec![MSG_DATA_INCOMING]);
        assert_eq!(packet.pdus[1][0], MSG_DATA);
        assert_eq!(packet.pdus[1].len(), 9);
    }

    #[test]
    fn callback_drops_new_audio_when_the_bounded_queue_is_full() {
        let (sender, receiver) = mpsc::sync_channel(1);
        enqueue_samples([0.25].into_iter(), 48_000, 1, &sender);
        enqueue_samples([0.75].into_iter(), 48_000, 1, &sender);

        assert_eq!(
            receiver
                .try_recv()
                .expect("first callback chunk is queued")
                .samples,
            vec![0.25]
        );
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
    }
}
