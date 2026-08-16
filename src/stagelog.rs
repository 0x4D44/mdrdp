//! Capture connection-stage transitions from IronRDP's own instrumentation.
//!
//! `ironrdp-blocking` emits `debug!(connector.state = <name>, …)` on every step of the
//! connection sequence, so the real stage sequence — Credssp, BasicSettingsExchange,
//! LicensingExchange, CapabilitiesExchange, ConnectionFinalization — is already reported
//! upstream. Reading it here is far better than reimplementing the connector's loop just
//! to time it: the crypto sequencing stays where it is tested.
//!
//! **Allowlist, not denylist.** Exactly one field is captured, `connector.state`, whose
//! value is a `&'static str` from a fixed set of state names. Every other field and
//! message is discarded without being read. Payload-free by construction, so no filter
//! has to be kept correct as upstream adds log lines.

use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing::field::{Field, Visit};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

/// The one field we read. Anything else is ignored.
const STATE_FIELD: &str = "connector.state";

/// Module whose activity proves the licensing exchange ran.
///
/// Licensing does not appear as a connector *state* when a message channel is present —
/// IronRDP advances it inline during `SecureSettingsExchange` and never enters
/// `ClientConnectorState::LicensingExchange`. But the module still emits events, and an
/// event's **target** is a compile-time module path: it cannot contain session data,
/// keys, or user input under any circumstances. Observing the target is therefore a
/// payload-free proof that licensing occurred.
///
/// This matters because that module also emits `trace!(?encryption_data, …)` carrying
/// licensing key material. We record that the module *spoke*, never what it said.
const LICENSING_TARGET: &str = "ironrdp_connector::license_exchange";

/// Name given to the synthetic stage recorded when licensing activity is observed.
pub const LICENSING_STAGE: &str = "LicensingExchange (inline)";

/// Module targets whose activity is itself the milestone worth recording, mapped to the
/// stage name to record. Targets are compile-time module paths, so this can never carry
/// session data — which is what lets us observe modules that log payloads without
/// reading any of them.
const TARGET_STAGES: &[(&str, &str)] = &[
    (LICENSING_TARGET, LICENSING_STAGE),
    ("ironrdp_dvc::client", "DVC channel (drdynvc)"),
    ("ironrdp_egfx::client", "EGFX graphics pipeline"),
];

#[derive(Debug, Clone, Serialize)]
pub struct StageEvent {
    /// An IronRDP `ClientConnectorState` name.
    pub state: String,
    /// Milliseconds since the previous stage transition.
    pub elapsed_ms: f64,
}

#[derive(Debug, Default)]
struct Inner {
    events: Vec<StageEvent>,
    last: Option<Instant>,
    /// Mirrors each *transition* to a live listener (a connect progress UI). Send
    /// failures are ignored; display must never fail the connect being displayed.
    live: Option<std::sync::mpsc::Sender<crate::connect::LiveStage>>,
}

/// Shared handle to the collected stages.
#[derive(Debug, Clone, Default)]
pub struct StageLog(Arc<Mutex<Inner>>);

impl StageLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// A stage log that also streams each transition to `live`.
    pub fn with_sender(live: std::sync::mpsc::Sender<crate::connect::LiveStage>) -> Self {
        let log = Self::default();
        log.0.lock().expect("stage log mutex").live = Some(live);
        log
    }

    fn record(&self, state: &str) {
        let now = Instant::now();
        let mut inner = self.0.lock().expect("stage log mutex");
        let elapsed_ms = inner
            .last
            .map(|t| (now - t).as_micros() as f64 / 1000.0)
            .unwrap_or(0.0);
        inner.last = Some(now);

        // Consecutive steps often report the same state (send, then wait). Collapse
        // them: the transition is what carries meaning, not the number of syscalls.
        if inner.events.last().map(|e| e.state.as_str()) == Some(state) {
            if let Some(last) = inner.events.last_mut() {
                last.elapsed_ms += elapsed_ms;
            }
            return;
        }

        if let Some(live) = &inner.live {
            let _ = live.send(crate::connect::LiveStage {
                name: state.to_owned(),
                elapsed_ms,
                qualifier: None,
            });
        }
        inner.events.push(StageEvent {
            state: state.to_owned(),
            elapsed_ms,
        });
    }

    /// Start the clock, so the first stage's elapsed time is measured from here.
    pub fn start(&self) {
        self.0.lock().expect("stage log mutex").last = Some(Instant::now());
    }

    pub fn take(&self) -> Vec<StageEvent> {
        std::mem::take(&mut self.0.lock().expect("stage log mutex").events)
    }
}

/// Pulls `connector.state` out of an event and discards everything else.
struct StateVisitor(Option<String>);

impl Visit for StateVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == STATE_FIELD {
            self.0 = Some(value.to_owned());
        }
    }

    /// Deliberately does nothing for every other field type.
    ///
    /// `record_debug` is where formatted PDU contents would arrive. Not reading it is
    /// what keeps session data out of the trace.
    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
}

impl<S: tracing::Subscriber> Layer<S> for StageLog {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        // Target first: some stages are proved by which module spoke, not by any field.
        // `target()` is `&'static str` from the call site, so no payload can reach here.
        let target = event.metadata().target();
        if let Some((_, stage)) = TARGET_STAGES.iter().find(|(t, _)| *t == target) {
            self.record(stage);
            return;
        }

        let mut visitor = StateVisitor(None);
        event.record(&mut visitor);
        if let Some(state) = visitor.0 {
            self.record(&state);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt as _;

    fn with_log<F: FnOnce()>(f: F) -> Vec<StageEvent> {
        let log = StageLog::new();
        let subscriber = tracing_subscriber::registry().with(log.clone());
        log.start();
        tracing::subscriber::with_default(subscriber, f);
        log.take()
    }

    #[test]
    fn captures_the_state_field_from_events() {
        let stages = with_log(|| {
            tracing::debug!(connector.state = "Credssp", "Wait for PDU");
            tracing::debug!(connector.state = "LicensingExchange", "Wait for PDU");
            tracing::debug!(connector.state = "CapabilitiesExchange", "Wait for PDU");
        });
        let names: Vec<&str> = stages.iter().map(|s| s.state.as_str()).collect();
        assert_eq!(
            names,
            ["Credssp", "LicensingExchange", "CapabilitiesExchange"]
        );
    }

    #[test]
    fn collapses_repeated_states_into_one_stage() {
        let stages = with_log(|| {
            tracing::debug!(connector.state = "Credssp", "step");
            tracing::debug!(connector.state = "Credssp", "step");
            tracing::debug!(connector.state = "Credssp", "step");
            tracing::debug!(connector.state = "LicensingExchange", "step");
        });
        assert_eq!(stages.len(), 2, "repeated states must collapse: {stages:?}");
        assert_eq!(stages[0].state, "Credssp");
        assert_eq!(stages[1].state, "LicensingExchange");
    }

    #[test]
    fn ignores_every_field_except_the_state() {
        // The whole safety argument. If any of these reached the trace, session data
        // and credentials could too.
        let stages = with_log(|| {
            tracing::trace!(length = 4096, "PDU received");
            tracing::trace!(response_len = 512, "Send response");
            tracing::debug!(password = "hunter2", "something careless upstream");
            tracing::debug!(pdu = ?"secret session content", "PDU");
            tracing::debug!(connector.state = "Credssp", "step");
        });
        assert_eq!(stages.len(), 1, "only the state event should be recorded");
        assert_eq!(stages[0].state, "Credssp");

        let rendered = serde_json::to_string(&stages).expect("serialise");
        assert!(!rendered.contains("hunter2"), "leaked a field: {rendered}");
        assert!(!rendered.contains("secret session"), "leaked: {rendered}");
        assert!(
            !rendered.contains("4096"),
            "leaked a size field: {rendered}"
        );
    }

    #[test]
    fn licensing_is_recorded_from_the_module_target_not_a_state() {
        // Licensing never appears as a connector state when a message channel is
        // present, so the trace proves it happened by observing which module spoke.
        let stages = with_log(|| {
            tracing::debug!(connector.state = "Credssp", "step");
            tracing::info!(target: "ironrdp_connector::license_exchange", "Client licensing completed");
            tracing::debug!(connector.state = "CapabilitiesExchange", "step");
        });
        let names: Vec<&str> = stages.iter().map(|s| s.state.as_str()).collect();
        assert_eq!(
            names,
            ["Credssp", LICENSING_STAGE, "CapabilitiesExchange"],
            "licensing must appear, in sequence"
        );
    }

    #[test]
    fn licensing_key_material_never_reaches_the_trace() {
        // ironrdp_connector::license_exchange emits trace!(?encryption_data, ...) which
        // carries licensing key material. We record that the module spoke, never what
        // it said — this test is the guard on that distinction.
        let stages = with_log(|| {
            tracing::trace!(
                target: "ironrdp_connector::license_exchange",
                encryption_data = "SUPER-SECRET-LICENSE-KEY",
                "Successfully generated Client License Info"
            );
            tracing::debug!(
                target: "ironrdp_connector::license_exchange",
                message = ?"a challenge PDU body",
                "Received"
            );
        });
        assert_eq!(
            stages.len(),
            1,
            "both events collapse to one licensing stage"
        );
        assert_eq!(stages[0].state, LICENSING_STAGE);

        let rendered = serde_json::to_string(&stages).expect("serialise");
        for forbidden in ["SUPER-SECRET", "LICENSE-KEY", "challenge PDU"] {
            assert!(
                !rendered.contains(forbidden),
                "leaked {forbidden:?} into the trace: {rendered}"
            );
        }
    }

    #[test]
    fn an_unrelated_module_target_is_not_mistaken_for_licensing() {
        let stages = with_log(|| {
            tracing::info!(target: "ironrdp_connector::license_exchange_helper", "not licensing");
            tracing::info!(target: "some_other::license_exchange", "also not");
        });
        assert!(
            stages.is_empty(),
            "only the exact target counts: {stages:?}"
        );
    }

    #[test]
    fn records_nothing_when_no_state_events_occur() {
        let stages = with_log(|| tracing::info!("unrelated"));
        assert!(stages.is_empty());
    }
}
