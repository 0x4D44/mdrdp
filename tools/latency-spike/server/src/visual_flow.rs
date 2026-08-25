//! Epoch-scoped, bounded admission for logical visual updates.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The maximum number of logical updates that may be reserved for one viewer.
pub const MAX_OUTSTANDING: usize = 2;
/// The capture loop's bounded condition-variable polling slice.
pub const WAIT_SLICE: Duration = Duration::from_millis(1);
const ACK_HISTORY_LIMIT: usize = 16;

pub type Epoch = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryState {
    Tentative,
    Admitted,
}

#[derive(Debug)]
struct Entry {
    state: EntryState,
    reserved_at: Instant,
}

#[derive(Debug)]
struct State {
    epoch: Epoch,
    entries: BTreeMap<u64, Entry>,
    acknowledged_sequences: BTreeSet<u64>,
    acknowledged: u64,
    cancelled: u64,
    observed_max: usize,
    waits: u64,
    wait_time: Duration,
    captures_avoided: u64,
    generation: u64,
}

impl Default for State {
    fn default() -> Self {
        Self {
            epoch: 0,
            entries: BTreeMap::new(),
            acknowledged_sequences: BTreeSet::new(),
            acknowledged: 0,
            cancelled: 0,
            observed_max: 0,
            waits: 0,
            wait_time: Duration::ZERO,
            captures_avoided: 0,
            generation: 0,
        }
    }
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

/// Cloneable flow state. Epoch and reservation changes always happen under one
/// mutex, and every mutation wakes capture waiters immediately.
#[derive(Clone)]
pub struct VisualFlow {
    shared: Arc<Shared>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReserveError {
    StaleEpoch { epoch: Epoch },
    Full,
    Duplicate { frame_seq: u64 },
    AlreadyAcknowledged { frame_seq: u64 },
}

impl std::fmt::Display for ReserveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "visual flow reservation: {self:?}")
    }
}

impl std::error::Error for ReserveError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservationError {
    StaleEpoch { epoch: Epoch },
    Unknown { frame_seq: u64 },
    AlreadyDisarmed { frame_seq: u64 },
}

impl std::fmt::Display for ReservationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "visual flow reservation token: {self:?}")
    }
}

impl std::error::Error for ReservationError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckError {
    StaleEpoch { epoch: Epoch },
    NotAdmitted { frame_seq: u64 },
    Duplicate { frame_seq: u64 },
    Unknown { frame_seq: u64 },
}

impl std::fmt::Display for AckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "visual flow acknowledgement: {self:?}")
    }
}

impl std::error::Error for AckError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckResult {
    Released,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservationState {
    Tentative,
    Admitted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitOutcome {
    pub changed: bool,
    pub timed_out: bool,
    pub epoch: Epoch,
    pub outstanding: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualFlowSnapshot {
    pub epoch: Epoch,
    pub outstanding: usize,
    pub tentative: usize,
    pub admitted: usize,
    pub observed_max: usize,
    pub acknowledged: u64,
    pub cancelled: u64,
    pub waits: u64,
    pub wait_time: Duration,
    pub captures_avoided: u64,
    pub oldest_frame_seq: Option<u64>,
    pub oldest_age: Duration,
    pub oldest_state: Option<ReservationState>,
}

/// A reservation starts tentative. Dropping it before queue admission releases
/// its permit; [`Reservation::admit`] transfers ownership to the exact ACK.
pub struct Reservation {
    flow: VisualFlow,
    epoch: Epoch,
    frame_seq: u64,
    armed: bool,
}

impl std::fmt::Debug for Reservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reservation")
            .field("epoch", &self.epoch)
            .field("frame_seq", &self.frame_seq)
            .field("armed", &self.armed)
            .finish()
    }
}

impl Reservation {
    pub fn is_armed(&self) -> bool {
        self.armed
    }

    /// Mark queue admission successful. The exact frame remains ACK-owned.
    pub fn admit(&mut self) -> Result<(), ReservationError> {
        if !self.armed {
            return Err(ReservationError::AlreadyDisarmed {
                frame_seq: self.frame_seq,
            });
        }
        let result = self
            .flow
            .admit(self.epoch, self.frame_seq)
            .map(|()| self.armed = false);
        if result.is_err() {
            // The epoch may have been reset while an encoder was finishing. The
            // reservation is already gone in that case, so Drop must not retry it.
            self.armed = false;
        }
        result
    }

    /// Explicitly cancel either a tentative or already admitted reservation.
    pub fn cancel(&mut self) -> Result<(), ReservationError> {
        if !self.armed {
            let result = self.flow.cancel(self.epoch, self.frame_seq);
            if result.is_ok() {
                self.armed = false;
            }
            return result;
        }
        self.armed = false;
        self.flow.cancel(self.epoch, self.frame_seq)
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.flow.cancel(self.epoch, self.frame_seq);
        }
    }
}

impl Default for VisualFlow {
    fn default() -> Self {
        Self::new()
    }
}

impl VisualFlow {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                changed: Condvar::new(),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn bump_generation(state: &mut State) {
        state.generation = state.generation.wrapping_add(1);
    }

    fn notify(&self) {
        self.shared.changed.notify_all();
    }

    /// Start a fresh viewer epoch, clearing all reservations and duplicate history.
    pub fn begin_epoch(&self) -> Epoch {
        self.reset()
    }

    /// Reset after disconnect or timeout and notify every capture waiter.
    pub fn reset(&self) -> Epoch {
        let next_epoch = {
            let mut state = self.lock();
            state.epoch = state.epoch.wrapping_add(1);
            state.entries.clear();
            state.acknowledged_sequences.clear();
            Self::bump_generation(&mut state);
            state.epoch
        };
        self.notify();
        next_epoch
    }

    /// Retire a viewer only when the caller still owns the current epoch.
    ///
    /// A stale feedback reader must not clear a successor's reservations, so
    /// the comparison and reset happen under the same state mutex.
    pub fn disconnect(&self, epoch: Epoch) -> bool {
        let reset = {
            let mut state = self.lock();
            if state.epoch != epoch {
                false
            } else {
                state.epoch = state.epoch.wrapping_add(1);
                state.entries.clear();
                state.acknowledged_sequences.clear();
                Self::bump_generation(&mut state);
                true
            }
        };
        if reset {
            self.notify();
        }
        reset
    }

    pub fn current_epoch(&self) -> Epoch {
        self.lock().epoch
    }

    pub fn outstanding(&self) -> usize {
        self.lock().entries.len()
    }

    pub fn is_full_for(&self, epoch: Epoch) -> bool {
        let state = self.lock();
        state.epoch == epoch && state.entries.len() >= MAX_OUTSTANDING
    }

    /// Account for a capture avoided because both visual permits were occupied.
    pub fn note_capture_skipped(&self) {
        let mut state = self.lock();
        state.captures_avoided = state.captures_avoided.saturating_add(1);
    }

    /// Reserve one exact logical update before any outbound queue or encoder work.
    pub fn reserve(&self, epoch: Epoch, frame_seq: u64) -> Result<Reservation, ReserveError> {
        let now = Instant::now();
        {
            let mut state = self.lock();
            if state.epoch != epoch {
                return Err(ReserveError::StaleEpoch { epoch });
            }
            if state.entries.contains_key(&frame_seq) {
                return Err(ReserveError::Duplicate { frame_seq });
            }
            if state.acknowledged_sequences.contains(&frame_seq) {
                return Err(ReserveError::AlreadyAcknowledged { frame_seq });
            }
            if state.entries.len() >= MAX_OUTSTANDING {
                return Err(ReserveError::Full);
            }
            state.entries.insert(
                frame_seq,
                Entry {
                    state: EntryState::Tentative,
                    reserved_at: now,
                },
            );
            state.observed_max = state.observed_max.max(state.entries.len());
            Self::bump_generation(&mut state);
        }
        self.notify();
        Ok(Reservation {
            flow: self.clone(),
            epoch,
            frame_seq,
            armed: true,
        })
    }

    fn admit(&self, epoch: Epoch, frame_seq: u64) -> Result<(), ReservationError> {
        let mut state = self.lock();
        if state.epoch != epoch {
            return Err(ReservationError::StaleEpoch { epoch });
        }
        let Some(entry) = state.entries.get_mut(&frame_seq) else {
            return Err(ReservationError::Unknown { frame_seq });
        };
        if entry.state != EntryState::Tentative {
            return Err(ReservationError::AlreadyDisarmed { frame_seq });
        }
        entry.state = EntryState::Admitted;
        Self::bump_generation(&mut state);
        drop(state);
        self.notify();
        Ok(())
    }

    fn cancel(&self, epoch: Epoch, frame_seq: u64) -> Result<(), ReservationError> {
        let mut state = self.lock();
        if state.epoch != epoch {
            return Err(ReservationError::StaleEpoch { epoch });
        }
        if state.entries.remove(&frame_seq).is_none() {
            return Err(ReservationError::Unknown { frame_seq });
        }
        state.cancelled = state.cancelled.saturating_add(1);
        Self::bump_generation(&mut state);
        drop(state);
        self.notify();
        Ok(())
    }

    /// Validate and release exactly one ACK-owned sequence.
    pub fn ack(&self, epoch: Epoch, frame_seq: u64) -> Result<AckResult, AckError> {
        let mut state = self.lock();
        if state.epoch != epoch {
            return Err(AckError::StaleEpoch { epoch });
        }
        match state.entries.get(&frame_seq).map(|entry| entry.state) {
            Some(EntryState::Tentative) => return Err(AckError::NotAdmitted { frame_seq }),
            Some(EntryState::Admitted) => {
                state.entries.remove(&frame_seq);
            }
            None if state.acknowledged_sequences.contains(&frame_seq) => {
                return Err(AckError::Duplicate { frame_seq });
            }
            None => return Err(AckError::Unknown { frame_seq }),
        }
        state.acknowledged_sequences.insert(frame_seq);
        while state.acknowledged_sequences.len() > ACK_HISTORY_LIMIT {
            state.acknowledged_sequences.pop_first();
        }
        state.acknowledged = state.acknowledged.saturating_add(1);
        Self::bump_generation(&mut state);
        drop(state);
        self.notify();
        Ok(AckResult::Released)
    }

    /// Wait for one state change, normally with [`WAIT_SLICE`]. The method does
    /// no polling loop or thread sleep: one condvar wait is all it performs.
    pub fn wait_slice(&self, epoch: Epoch, timeout: Duration) -> WaitOutcome {
        let started = Instant::now();
        let state = self.lock();
        let initial_generation = state.generation;
        if state.epoch != epoch {
            return WaitOutcome {
                changed: true,
                timed_out: false,
                epoch: state.epoch,
                outstanding: state.entries.len(),
            };
        }
        let (mut state, wait_result) = self
            .shared
            .changed
            .wait_timeout(state, timeout)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let elapsed = started.elapsed();
        state.waits = state.waits.saturating_add(1);
        state.wait_time = state.wait_time.saturating_add(elapsed);
        WaitOutcome {
            changed: state.generation != initial_generation || state.epoch != epoch,
            timed_out: wait_result.timed_out(),
            epoch: state.epoch,
            outstanding: state.entries.len(),
        }
    }

    pub fn snapshot(&self) -> VisualFlowSnapshot {
        let state = self.lock();
        let mut tentative = 0;
        let mut admitted = 0;
        let mut oldest = None;
        for (&frame_seq, entry) in &state.entries {
            match entry.state {
                EntryState::Tentative => tentative += 1,
                EntryState::Admitted => admitted += 1,
            }
            if oldest.is_none_or(|(_, _, at)| entry.reserved_at < at) {
                oldest = Some((frame_seq, entry.state, entry.reserved_at));
            }
        }
        let (oldest_frame_seq, oldest_state, oldest_age) = match oldest {
            Some((frame_seq, state_kind, reserved_at)) => (
                Some(frame_seq),
                Some(match state_kind {
                    EntryState::Tentative => ReservationState::Tentative,
                    EntryState::Admitted => ReservationState::Admitted,
                }),
                reserved_at.elapsed(),
            ),
            None => (None, None, Duration::ZERO),
        };
        VisualFlowSnapshot {
            epoch: state.epoch,
            outstanding: state.entries.len(),
            tentative,
            admitted,
            observed_max: state.observed_max,
            acknowledged: state.acknowledged,
            cancelled: state.cancelled,
            waits: state.waits,
            wait_time: state.wait_time,
            captures_avoided: state.captures_avoided,
            oldest_frame_seq,
            oldest_age,
            oldest_state,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn capacity_is_two_and_exact_out_of_order_ack_frees_only_that_sequence() {
        let flow = VisualFlow::new();
        let epoch = flow.begin_epoch();
        let mut first = flow.reserve(epoch, 10).unwrap();
        let mut second = flow.reserve(epoch, 20).unwrap();
        first.admit().unwrap();
        second.admit().unwrap();
        assert!(matches!(flow.reserve(epoch, 30), Err(ReserveError::Full)));

        assert_eq!(flow.ack(epoch, 20), Ok(AckResult::Released));
        assert_eq!(flow.outstanding(), 1);
        assert_eq!(flow.ack(epoch, 10), Ok(AckResult::Released));
        assert_eq!(flow.outstanding(), 0);
    }

    #[test]
    fn duplicate_and_unknown_acknowledgements_are_rejected() {
        let flow = VisualFlow::new();
        let epoch = flow.begin_epoch();
        let mut reservation = flow.reserve(epoch, 7).unwrap();
        reservation.admit().unwrap();
        assert_eq!(flow.ack(epoch, 7), Ok(AckResult::Released));
        assert_eq!(
            flow.ack(epoch, 7),
            Err(AckError::Duplicate { frame_seq: 7 })
        );
        assert_eq!(flow.ack(epoch, 8), Err(AckError::Unknown { frame_seq: 8 }));
    }

    #[test]
    fn old_epoch_ack_is_rejected_without_touching_the_new_epoch() {
        let flow = VisualFlow::new();
        let old = flow.begin_epoch();
        let mut old_reservation = flow.reserve(old, 11).unwrap();
        old_reservation.admit().unwrap();
        let current = flow.begin_epoch();
        let mut current_reservation = flow.reserve(current, 22).unwrap();
        current_reservation.admit().unwrap();

        assert_eq!(flow.ack(old, 11), Err(AckError::StaleEpoch { epoch: old }));
        assert_eq!(flow.outstanding(), 1);
        assert_eq!(flow.ack(current, 22), Ok(AckResult::Released));
    }

    #[test]
    fn stale_disconnect_cannot_reset_a_successor_epoch() {
        let flow = VisualFlow::new();
        let old = flow.begin_epoch();
        let current = flow.begin_epoch();
        let mut reservation = flow.reserve(current, 33).unwrap();
        reservation.admit().unwrap();
        assert!(!flow.disconnect(old));
        assert_eq!(flow.outstanding(), 1);
        assert!(flow.disconnect(current));
        assert_eq!(flow.outstanding(), 0);
    }

    #[test]
    fn dropping_a_tentative_reservation_cancels_it() {
        let flow = VisualFlow::new();
        let epoch = flow.begin_epoch();
        {
            let _reservation = flow.reserve(epoch, 1).unwrap();
            assert_eq!(flow.outstanding(), 1);
        }
        assert_eq!(flow.outstanding(), 0);
        assert!(flow.reserve(epoch, 2).is_ok());
    }

    #[test]
    fn admission_retains_an_ack_owned_reservation_after_token_drop() {
        let flow = VisualFlow::new();
        let epoch = flow.begin_epoch();
        let mut reservation = flow.reserve(epoch, 4).unwrap();
        reservation.admit().unwrap();
        drop(reservation);
        assert_eq!(flow.outstanding(), 1);
        assert_eq!(flow.ack(epoch, 4), Ok(AckResult::Released));
    }

    #[test]
    fn reset_clears_reservations_and_wakes_a_full_window_waiter() {
        let flow = Arc::new(VisualFlow::new());
        let epoch = flow.begin_epoch();
        let mut first = flow.reserve(epoch, 1).unwrap();
        let mut second = flow.reserve(epoch, 2).unwrap();
        first.admit().unwrap();
        second.admit().unwrap();

        let waiter_flow = Arc::clone(&flow);
        let started = Instant::now();
        let waiter = thread::spawn(move || waiter_flow.wait_slice(epoch, Duration::from_secs(5)));
        thread::sleep(Duration::from_millis(5));
        let next_epoch = flow.reset();
        assert_ne!(next_epoch, epoch);
        let outcome = waiter.join().unwrap();
        assert!(outcome.changed || started.elapsed() < Duration::from_secs(1));
        assert_eq!(flow.outstanding(), 0);
        assert!(flow.reserve(next_epoch, 3).is_ok());
    }

    #[test]
    fn oldest_age_and_counters_report_admission_progress() {
        let flow = VisualFlow::new();
        let epoch = flow.begin_epoch();
        let mut reservation = flow.reserve(epoch, 10).unwrap();
        reservation.admit().unwrap();
        let snapshot = flow.snapshot();
        assert_eq!(snapshot.outstanding, 1);
        assert_eq!(snapshot.observed_max, 1);
        assert_eq!(snapshot.acknowledged, 0);
        assert_eq!(snapshot.oldest_frame_seq, Some(10));
        assert!(snapshot.oldest_age <= Duration::from_secs(1));
        flow.note_capture_skipped();
        assert_eq!(flow.snapshot().captures_avoided, 1);
        flow.ack(epoch, 10).unwrap();
        assert_eq!(flow.snapshot().acknowledged, 1);
    }

    #[test]
    fn acknowledgement_duplicate_history_stays_bounded() {
        let flow = VisualFlow::new();
        let epoch = flow.begin_epoch();
        for seq in 1..=100 {
            let mut reservation = flow.reserve(epoch, seq).unwrap();
            reservation.admit().unwrap();
            flow.ack(epoch, seq).unwrap();
        }
        assert_eq!(flow.lock().acknowledged_sequences.len(), 16);
    }
}
