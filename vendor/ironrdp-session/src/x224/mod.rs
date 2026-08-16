use ironrdp_core::{WriteBuf, decode};
use ironrdp_dvc::{DrdynvcClient, DvcProcessor, DynamicVirtualChannel};
use ironrdp_pdu::mcs::{DisconnectProviderUltimatum, DisconnectReason, McsMessage, SendDataIndicationCtx};
use ironrdp_pdu::rdp::autodetect::{AutoDetectReqPdu, AutoDetectRequest, AutoDetectResponse, AutoDetectRspPdu};
use ironrdp_pdu::rdp::headers::ShareDataPdu;
use ironrdp_pdu::rdp::multitransport::MultitransportRequestPdu;
use ironrdp_pdu::rdp::server_error_info::{ErrorInfo, ProtocolIndependentCode, ServerSetErrorInfoPdu};
use ironrdp_pdu::x224::X224;
use ironrdp_svc::{StaticChannelSet, SvcMessage, SvcProcessor, SvcProcessorMessages, client_encode_svc_messages};
use tracing::debug;

use crate::{SessionError, SessionErrorExt as _, SessionResult, reason_err};

/// X224 Processor output
#[derive(Debug, Clone)]
pub enum ProcessorOutput {
    /// A buffer with encoded data to send to the server.
    ResponseFrame(Vec<u8>),
    /// A graceful disconnect notification. Client should close the connection upon receiving this.
    Disconnect(DisconnectDescription),
    /// Received a [`ironrdp_pdu::rdp::headers::ServerDeactivateAll`] PDU. Client should execute the
    /// [Deactivation-Reactivation Sequence].
    ///
    /// [Deactivation-Reactivation Sequence]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/dfc234ce-481a-4674-9a5d-2a7bafb14432
    DeactivateAll,
    /// Server Initiate Multitransport Request. The application should establish a
    /// sideband UDP transport using the request ID and security cookie, then send
    /// a [`MultitransportResponsePdu`] back on the IO channel.
    ///
    /// See [\[MS-RDPBCGR\] 2.2.15.1].
    ///
    /// [\[MS-RDPBCGR\] 2.2.15.1]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/de783158-8b01-4818-8fb0-62523a5b3490
    /// [`MultitransportResponsePdu`]: ironrdp_pdu::rdp::multitransport::MultitransportResponsePdu
    MultitransportRequest(MultitransportRequestPdu),
    /// Auto-detect network characteristics from server ([\[MS-RDPBCGR\] 2.2.14]).
    ///
    /// Currently only surfaces [`AutoDetectRequest::NetworkCharacteristicsResult`].
    /// RTT requests are handled internally with automatic responses.
    ///
    /// [\[MS-RDPBCGR\] 2.2.14]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/dc672839-4f4e-40b1-a71c-cd6a959baa38
    AutoDetect(AutoDetectRequest),
    /// Slow-path graphics update ([MS-RDPBCGR] 2.2.9.1.1.3).
    /// Raw update payload starting with `updateType(u16)`.
    GraphicsUpdate(Vec<u8>),
    /// Slow-path pointer update ([MS-RDPBCGR] 2.2.9.1.1.4).
    /// Raw pointer payload starting with `messageType(u16) + pad(u16)`.
    PointerUpdate(Vec<u8>),
}

#[derive(Debug, Clone)]
pub enum DisconnectDescription {
    /// Includes the reason from the MCS Disconnect Provider Ultimatum.
    /// This is the least-specific disconnect reason and is only used
    /// when a more specific disconnect code is not available.
    McsDisconnect(DisconnectReason),

    /// Includes the error information sent by the RDP server when there
    /// is a connection or disconnection failure.
    ErrorInfo(ErrorInfo),
}

pub struct Processor {
    static_channels: StaticChannelSet,
    user_channel_id: u16,
    io_channel_id: u16,
    message_channel_id: Option<u16>,
    share_id: u32,
    bandwidth_measure: Option<BandwidthMeasure>,
}

/// An in-progress bandwidth measurement ([MS-RDPBCGR] 2.2.14.1.2 – 2.2.14.1.4).
///
/// The server brackets a stretch of its own traffic with Bandwidth Measure Start and
/// Stop, and the client reports back how many bytes arrived in how much time. The
/// byte count covers EVERY PDU received on the connection while the measurement is
/// running — fast-path graphics included, which is the bulk of it — so the caller
/// feeds every inbound frame length through [`Processor::register_inbound_bytes`].
/// This mirrors FreeRDP, which counts both TPKT and fast-path PDU lengths.
///
/// Ignoring these requests is not a safe default: a Windows server that never
/// receives Bandwidth Measure Results keeps re-probing and throttles the graphics
/// pipeline to a crawl, skipping frames mid-repaint.
struct BandwidthMeasure {
    started_at: std::time::Instant,
    byte_count: u64,
}

impl Processor {
    pub fn new(
        static_channels: StaticChannelSet,
        user_channel_id: u16,
        io_channel_id: u16,
        message_channel_id: Option<u16>,
        share_id: u32,
    ) -> Self {
        Self {
            static_channels,
            user_channel_id,
            io_channel_id,
            message_channel_id,
            share_id,
            bandwidth_measure: None,
        }
    }

    /// Count `len` bytes of inbound traffic toward a running bandwidth measurement.
    ///
    /// A no-op unless a Bandwidth Measure Start has arrived without its Stop yet.
    /// The caller invokes this for every frame received on the connection,
    /// whichever action it carries.
    pub fn register_inbound_bytes(&mut self, len: usize) {
        if let Some(measure) = &mut self.bandwidth_measure {
            measure.byte_count = measure.byte_count.saturating_add(len as u64);
        }
    }

    pub fn set_share_id(&mut self, share_id: u32) {
        self.share_id = share_id;
    }

    pub fn get_svc_processor<T: SvcProcessor + 'static>(&self) -> Option<&T> {
        self.static_channels
            .get_by_type::<T>()
            .and_then(|svc| svc.channel_processor_downcast_ref())
    }

    pub fn get_svc_processor_mut<T: SvcProcessor + 'static>(&mut self) -> Option<&mut T> {
        self.static_channels
            .get_by_type_mut::<T>()
            .and_then(|svc| svc.channel_processor_downcast_mut())
    }

    /// Completes user's SVC request with data, required to sent it over the network and returns
    /// a buffer with encoded data.
    pub fn process_svc_processor_messages<C: SvcProcessor + 'static>(
        &self,
        messages: SvcProcessorMessages<C>,
    ) -> SessionResult<Vec<u8>> {
        let channel_id = self
            .static_channels
            .get_channel_id_by_type::<C>()
            .ok_or_else(|| reason_err!("SVC", "channel not found"))?;

        process_svc_messages(messages.into(), channel_id, self.user_channel_id)
    }

    pub fn get_dvc<T: DvcProcessor + 'static>(&self) -> Option<&DynamicVirtualChannel> {
        self.get_svc_processor::<DrdynvcClient>()?.get_dvc_by_type_id::<T>()
    }

    pub fn get_dvc_by_channel_id(&self, channel_id: u32) -> Option<&DynamicVirtualChannel> {
        self.get_svc_processor::<DrdynvcClient>()?
            .get_dvc_by_channel_id(channel_id)
    }

    /// Processes a received PDU. Returns a vector of [`ProcessorOutput`] that must be processed
    /// in the returned order.
    pub fn process(&mut self, frame: &[u8]) -> SessionResult<Vec<ProcessorOutput>> {
        let data_ctx: SendDataIndicationCtx<'_> =
            ironrdp_pdu::mcs::decode_send_data_indication(frame).map_err(SessionError::decode)?;
        let channel_id = data_ctx.channel_id;

        if channel_id == self.io_channel_id {
            self.process_io_channel(data_ctx)
        } else if self.message_channel_id == Some(channel_id) {
            self.process_message_channel(data_ctx)
        } else if let Some(svc) = self.static_channels.get_by_channel_id_mut(channel_id) {
            let response_pdus = svc.process(data_ctx.user_data).map_err(SessionError::pdu)?;
            process_svc_messages(response_pdus, channel_id, data_ctx.initiator_id)
                .map(|data| vec![ProcessorOutput::ResponseFrame(data)])
        } else {
            Err(reason_err!("X224", "unexpected channel received: ID {channel_id}"))
        }
    }

    fn process_io_channel(&self, data_ctx: SendDataIndicationCtx<'_>) -> SessionResult<Vec<ProcessorOutput>> {
        debug_assert_eq!(data_ctx.channel_id, self.io_channel_id);

        let io_channel = ironrdp_pdu::rdp::headers::decode_io_channel(data_ctx).map_err(SessionError::decode)?;

        match io_channel {
            ironrdp_pdu::rdp::headers::IoChannelPdu::Data(ctx) => {
                match ctx.pdu {
                    ShareDataPdu::SaveSessionInfo(session_info) => {
                        debug!("Got Session Save Info PDU: {session_info:?}");
                        Ok(Vec::new())
                    }
                    // FIXME: workaround fix to not terminate the session on "unhandled PDU: Set Keyboard Indicators PDU"
                    ShareDataPdu::SetKeyboardIndicators(data) => {
                        debug!("Got Keyboard Indicators PDU: {data:?}");
                        Ok(Vec::new())
                    }
                    ShareDataPdu::ServerSetErrorInfo(ServerSetErrorInfoPdu(ErrorInfo::ProtocolIndependentCode(
                        ProtocolIndependentCode::None,
                    ))) => {
                        debug!("Received None server error");
                        Ok(Vec::new())
                    }
                    ShareDataPdu::ServerSetErrorInfo(ServerSetErrorInfoPdu(e)) => {
                        // This is a part of server-side graceful disconnect procedure defined
                        // in [MS-RDPBCGR].
                        //
                        // [MS-RDPBCGR]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/149070b0-ecec-4c20-af03-934bbc48adb8
                        let desc = DisconnectDescription::ErrorInfo(e);
                        Ok(vec![ProcessorOutput::Disconnect(desc)])
                    }
                    ShareDataPdu::ShutdownDenied => {
                        debug!("ShutdownDenied received, session will be closed");

                        // As defined in [MS-RDPBCGR], when `ShareDataPdu::ShutdownDenied` is received, we
                        // need to send a disconnect ultimatum to the server if we want to proceed with the
                        // session shutdown.
                        //
                        // [MS-RDPBCGR]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/27915739-8f77-487e-9927-55008af7fd68
                        let ultimatum = McsMessage::DisconnectProviderUltimatum(
                            DisconnectProviderUltimatum::from_reason(DisconnectReason::UserRequested),
                        );

                        let encoded_pdu = ironrdp_core::encode_vec(&X224(ultimatum)).map_err(SessionError::encode);

                        Ok(vec![
                            ProcessorOutput::ResponseFrame(encoded_pdu?),
                            ProcessorOutput::Disconnect(DisconnectDescription::McsDisconnect(
                                DisconnectReason::UserRequested,
                            )),
                        ])
                    }
                    // TODO: slow-path payloads may be bulk-compressed when
                    // ClientInfoFlags::COMPRESSION is negotiated. Decompression
                    // should happen here before passing data downstream. Currently
                    // IronRDP does not wire bulk decompression into this path.
                    // FIXME: until this is wired, the client deliberately defaults to the simple,
                    // stateless-friendly MPPC 64K (RDP5) compression level rather than XCRUSH; a
                    // stateful codec would risk silent corruption on slow-path updates.
                    ShareDataPdu::Update(data) => {
                        debug!("Got slow-path graphics update ({} bytes)", data.len());
                        Ok(vec![ProcessorOutput::GraphicsUpdate(data)])
                    }
                    ShareDataPdu::Pointer(data) => {
                        debug!("Got slow-path pointer update ({} bytes)", data.len());
                        Ok(vec![ProcessorOutput::PointerUpdate(data)])
                    }
                    _ => Err(reason_err!(
                        "IO channel",
                        "unhandled PDU: {:?}",
                        ctx.pdu.as_short_name()
                    )),
                }
            }
            ironrdp_pdu::rdp::headers::IoChannelPdu::MultitransportRequest(pdu) => {
                debug!(
                    "Received Initiate Multitransport Request: request_id={}",
                    pdu.request_id
                );
                Ok(vec![ProcessorOutput::MultitransportRequest(pdu)])
            }
            ironrdp_pdu::rdp::headers::IoChannelPdu::DeactivateAll(_) => Ok(vec![ProcessorOutput::DeactivateAll]),
        }
    }

    /// Process an auto-detect request received on the MCS message channel.
    ///
    /// During continuous auto-detection ([MS-RDPBCGR] 2.2.14) the server sends
    /// RTT (and bandwidth) requests on the message channel; the client answers
    /// RTT requests and surfaces the final Network Characteristics Result.
    fn process_message_channel(&mut self, data_ctx: SendDataIndicationCtx<'_>) -> SessionResult<Vec<ProcessorOutput>> {
        let Some(message_channel_id) = self.message_channel_id else {
            return Err(reason_err!("message channel", "no message channel negotiated"));
        };

        let req = decode::<AutoDetectReqPdu>(data_ctx.user_data).map_err(SessionError::decode)?;

        match req.request {
            AutoDetectRequest::RttRequest { sequence_number, .. } => {
                let response = AutoDetectRspPdu::new(AutoDetectResponse::RttResponse { sequence_number });
                let frame = self.encode_message_channel_response(message_channel_id, &response)?;
                debug!(sequence_number, "Responded to auto-detect RTT request");
                Ok(vec![ProcessorOutput::ResponseFrame(frame)])
            }
            AutoDetectRequest::BandwidthMeasureStart { sequence_number, .. } => {
                // Restart, never accumulate: a second Start supersedes an unanswered one.
                self.bandwidth_measure = Some(BandwidthMeasure {
                    started_at: std::time::Instant::now(),
                    byte_count: 0,
                });
                debug!(sequence_number, "Bandwidth measurement started");
                Ok(Vec::new())
            }
            AutoDetectRequest::BandwidthMeasurePayload { sequence_number, payload } => {
                // Payload PDUs exist to generate measurable traffic; their bytes count
                // like any other inbound frame. `register_inbound_bytes` has already
                // counted this frame's transport length, so nothing further to add —
                // the arm exists so the payload is not logged as unimplemented.
                let _ = (sequence_number, payload);
                Ok(Vec::new())
            }
            AutoDetectRequest::BandwidthMeasureStop { sequence_number, request_type, .. } => {
                use ironrdp_pdu::rdp::autodetect::{BW_RESULTS_CONNECT_TIME, BW_RESULTS_CONTINUOUS, BW_STOP_CONNECT_TIME};

                // A Stop with no Start still gets a response: the server is waiting on
                // it, and an unanswered Stop leaves the server's estimator starved,
                // which it punishes by throttling and frame-skipping the whole
                // graphics pipeline.
                let (time_delta_ms, byte_count) = match self.bandwidth_measure.take() {
                    Some(measure) => (
                        u32::try_from(measure.started_at.elapsed().as_millis()).unwrap_or(u32::MAX),
                        u32::try_from(measure.byte_count).unwrap_or(u32::MAX),
                    ),
                    None => (0, 0),
                };
                let response_type = if request_type == BW_STOP_CONNECT_TIME {
                    BW_RESULTS_CONNECT_TIME
                } else {
                    BW_RESULTS_CONTINUOUS
                };
                let response = AutoDetectRspPdu::new(AutoDetectResponse::BandwidthMeasureResults {
                    sequence_number,
                    response_type,
                    time_delta_ms,
                    byte_count,
                });
                let frame = self.encode_message_channel_response(message_channel_id, &response)?;
                debug!(
                    sequence_number,
                    time_delta_ms, byte_count, "Responded to bandwidth measure stop"
                );
                Ok(vec![ProcessorOutput::ResponseFrame(frame)])
            }
            req @ AutoDetectRequest::NetworkCharacteristicsResult { .. } => {
                debug!(?req, "Received network characteristics from server");
                Ok(vec![ProcessorOutput::AutoDetect(req)])
            }
        }
    }

    fn encode_message_channel_response(
        &self,
        message_channel_id: u16,
        response: &AutoDetectRspPdu,
    ) -> SessionResult<Vec<u8>> {
        let mut frame = WriteBuf::new();
        ironrdp_pdu::mcs::encode_send_data_request(self.user_channel_id, message_channel_id, response, &mut frame)
            .map_err(SessionError::encode)?;
        Ok(frame.into_inner())
    }

    /// Send a pdu on the static global channel. Typically used to send input events
    pub fn encode_static(&self, output: &mut WriteBuf, pdu: ShareDataPdu) -> SessionResult<usize> {
        let written = ironrdp_pdu::rdp::headers::encode_share_data(
            self.user_channel_id,
            self.io_channel_id,
            self.share_id,
            pdu,
            output,
        )
        .map_err(SessionError::encode)?;
        Ok(written)
    }
}

/// Processes a vector of [`SvcMessage`] in preparation for sending them to the server on the `channel_id` channel.
///
/// This includes chunkifying the messages, adding MCS, x224, and tpkt headers, and encoding them into a buffer.
/// The messages returned here are ready to be sent to the server.
///
/// The caller is responsible for ensuring that the `channel_id` corresponds to the correct channel.
fn process_svc_messages(messages: Vec<SvcMessage>, channel_id: u16, initiator_id: u16) -> SessionResult<Vec<u8>> {
    client_encode_svc_messages(messages, channel_id, initiator_id).map_err(SessionError::encode)
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use ironrdp_core::encode_buf;
    use ironrdp_pdu::mcs::SendDataIndication;
    use ironrdp_pdu::rdp::autodetect::{BW_RESULTS_CONNECT_TIME, BW_RESULTS_CONTINUOUS, BW_STOP_CONNECT_TIME};

    use super::*;

    const MESSAGE_CHANNEL_ID: u16 = 1005;

    fn processor() -> Processor {
        Processor::new(StaticChannelSet::new(), 1002, 1003, Some(MESSAGE_CHANNEL_ID), 0x1_0000)
    }

    /// Encode a server-side Auto-Detect Request as the wire frame the processor receives.
    fn autodetect_frame(request: AutoDetectRequest) -> Vec<u8> {
        let pdu = AutoDetectReqPdu::new(request);
        let user_data = ironrdp_core::encode_vec(&pdu).expect("encode autodetect request");
        let indication = SendDataIndication {
            initiator_id: 1002,
            channel_id: MESSAGE_CHANNEL_ID,
            user_data: Cow::Owned(user_data),
        };
        let mut buf = WriteBuf::new();
        encode_buf(&X224(indication), &mut buf).expect("encode send data indication");
        buf.into_inner()
    }

    /// Decode the processor's response frame back into the auto-detect response it carries.
    fn decode_response(frame: &[u8]) -> AutoDetectResponse {
        use ironrdp_pdu::mcs::SendDataRequest;
        let msg = decode::<X224<McsMessage<'_>>>(frame).expect("decode response frame");
        let McsMessage::SendDataRequest(SendDataRequest { user_data, .. }) = msg.0 else {
            panic!("expected a send data request");
        };
        decode::<AutoDetectRspPdu>(&user_data)
            .expect("decode autodetect response")
            .response
    }

    #[test]
    fn bandwidth_stop_is_answered_with_measured_results() {
        // The regression this guards: bandwidth measure requests silently dropped, after
        // which a Windows server throttles and frame-skips the EGFX pipeline for the
        // rest of the session.
        let mut processor = processor();

        let outputs = processor
            .process(&autodetect_frame(AutoDetectRequest::BandwidthMeasureStart {
                sequence_number: 7,
                request_type: 0x0014,
            }))
            .expect("process bandwidth start");
        assert!(outputs.is_empty(), "a start expects no response");

        // The traffic the server wants measured.
        processor.register_inbound_bytes(40_000);
        processor.register_inbound_bytes(2_048);

        let outputs = processor
            .process(&autodetect_frame(AutoDetectRequest::BandwidthMeasureStop {
                sequence_number: 7,
                request_type: 0x0429,
                payload: None,
            }))
            .expect("process bandwidth stop");

        let [ProcessorOutput::ResponseFrame(frame)] = outputs.as_slice() else {
            panic!("a stop must produce exactly one response frame, got {outputs:?}");
        };
        let AutoDetectResponse::BandwidthMeasureResults {
            sequence_number,
            response_type,
            time_delta_ms: _,
            byte_count,
        } = decode_response(frame)
        else {
            panic!("expected bandwidth measure results");
        };
        assert_eq!(sequence_number, 7);
        assert_eq!(
            response_type, BW_RESULTS_CONTINUOUS,
            "a non-connect-time stop takes the continuous response type"
        );
        assert_eq!(byte_count, 42_048, "all inbound bytes since the start count");
    }

    #[test]
    fn a_connect_time_stop_takes_the_connect_time_response_type() {
        let mut processor = processor();
        processor
            .process(&autodetect_frame(AutoDetectRequest::BandwidthMeasureStart {
                sequence_number: 1,
                request_type: 0x0014,
            }))
            .expect("process start");
        let outputs = processor
            .process(&autodetect_frame(AutoDetectRequest::BandwidthMeasureStop {
                sequence_number: 1,
                request_type: BW_STOP_CONNECT_TIME,
                // A connect-time stop carries a payloadLength field on the wire, so the
                // payload is present-but-empty rather than absent.
                payload: Some(Vec::new()),
            }))
            .expect("process stop");
        let [ProcessorOutput::ResponseFrame(frame)] = outputs.as_slice() else {
            panic!("expected one response frame");
        };
        let AutoDetectResponse::BandwidthMeasureResults { response_type, .. } = decode_response(frame) else {
            panic!("expected bandwidth measure results");
        };
        assert_eq!(response_type, BW_RESULTS_CONNECT_TIME);
    }

    #[test]
    fn a_stop_without_a_start_still_answers_with_zeros() {
        // The server is blocked waiting on the response either way; zeros beat silence.
        let mut processor = processor();
        processor.register_inbound_bytes(999); // no measurement running: must not count
        let outputs = processor
            .process(&autodetect_frame(AutoDetectRequest::BandwidthMeasureStop {
                sequence_number: 3,
                request_type: 0x0429,
                payload: None,
            }))
            .expect("process stop");
        let [ProcessorOutput::ResponseFrame(frame)] = outputs.as_slice() else {
            panic!("expected one response frame");
        };
        let AutoDetectResponse::BandwidthMeasureResults {
            time_delta_ms,
            byte_count,
            ..
        } = decode_response(frame)
        else {
            panic!("expected bandwidth measure results");
        };
        assert_eq!((time_delta_ms, byte_count), (0, 0));
    }

    #[test]
    fn bytes_before_the_start_do_not_count() {
        let mut processor = processor();
        processor.register_inbound_bytes(10_000);
        processor
            .process(&autodetect_frame(AutoDetectRequest::BandwidthMeasureStart {
                sequence_number: 2,
                request_type: 0x0014,
            }))
            .expect("process start");
        processor.register_inbound_bytes(500);
        let outputs = processor
            .process(&autodetect_frame(AutoDetectRequest::BandwidthMeasureStop {
                sequence_number: 2,
                request_type: 0x0429,
                payload: None,
            }))
            .expect("process stop");
        let [ProcessorOutput::ResponseFrame(frame)] = outputs.as_slice() else {
            panic!("expected one response frame");
        };
        let AutoDetectResponse::BandwidthMeasureResults { byte_count, .. } = decode_response(frame) else {
            panic!("expected bandwidth measure results");
        };
        assert_eq!(byte_count, 500, "a start resets the counter");
    }
}
