//! Portable assembly of independently encoded tiles into complete desktop frames.

use std::collections::BTreeMap;

/// Decoder recovery after a complete logical frame is lost.
pub(crate) struct Recovery {
    waiting_for_keyframe: bool,
    dropped: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryDecision {
    Admit,
    Suppress,
    SuppressAndRequest,
}

impl Recovery {
    pub(crate) fn waiting() -> Self {
        Self {
            waiting_for_keyframe: true,
            dropped: 0,
        }
    }

    /// Account for incomplete assembled frames. Every expiry requests another
    /// keyframe because the expired set may have contained the previous request's
    /// only usable keyframe for one tile.
    pub(crate) fn drop_incomplete(&mut self, count: u64) {
        self.dropped += count;
        self.waiting_for_keyframe = true;
    }

    /// Account for a captured frame discarded before any encoder saw it. Decoder
    /// references remain intact, so this must not enter keyframe recovery.
    pub(crate) fn drop_before_encode(&mut self) {
        self.dropped += 1;
    }

    /// Decide whether a complete set may enter the queue. Delta sets produced while
    /// recovering are discarded as whole logical frames.
    pub(crate) fn prepare(&mut self, any_keyframe: bool, all_keyframes: bool) -> RecoveryDecision {
        if self.waiting_for_keyframe && !all_keyframes {
            self.dropped += 1;
            if any_keyframe {
                // The tile encoders answered the same request on different capture
                // sequences. Ask all of them again so recovery cannot wedge with
                // every per-encoder request already considered satisfied.
                RecoveryDecision::SuppressAndRequest
            } else {
                RecoveryDecision::Suppress
            }
        } else {
            RecoveryDecision::Admit
        }
    }

    /// A queue rejection loses the entire prepared set and always requires another
    /// keyframe: the rejected set may itself have been the recovery set.
    pub(crate) fn queue_full(&mut self) {
        self.dropped += 1;
        self.waiting_for_keyframe = true;
    }

    pub(crate) fn admitted(&mut self, all_keyframes: bool) {
        if all_keyframes {
            self.waiting_for_keyframe = false;
        }
    }

    pub(crate) fn reset(&mut self) {
        self.waiting_for_keyframe = true;
    }

    pub(crate) fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Rect overlays are deltas against the viewer's painted desktop. They are
    /// trustworthy only after a complete recovery keyframe entered the send queue.
    pub(crate) fn allows_overlays(&self) -> bool {
        !self.waiting_for_keyframe
    }

    #[cfg(test)]
    fn waiting_for_keyframe(&self) -> bool {
        self.waiting_for_keyframe
    }
}

pub(crate) struct Complete<T> {
    pub(crate) seq: u64,
    pub(crate) tiles: Vec<T>,
}

pub(crate) struct Push<T> {
    pub(crate) ready: Vec<Complete<T>>,
    pub(crate) dropped: u64,
}

struct Partial<T> {
    tiles: Vec<Option<T>>,
}

pub(crate) struct Assembler<T> {
    tile_count: usize,
    max_pending: usize,
    pending: BTreeMap<u64, Partial<T>>,
    retired_through: Option<u64>,
}

impl<T> Assembler<T> {
    pub(crate) fn new(tile_count: usize, max_pending: usize) -> Self {
        assert!(tile_count > 0);
        assert!(max_pending > 0);
        Self {
            tile_count,
            max_pending,
            pending: BTreeMap::new(),
            retired_through: None,
        }
    }

    pub(crate) fn push(&mut self, seq: u64, tile_id: u8, tile: T) -> Push<T> {
        if self.retired_through.is_some_and(|retired| seq <= retired) {
            return Push {
                ready: Vec::new(),
                dropped: 0,
            };
        }
        let tile_id = usize::from(tile_id);
        if tile_id >= self.tile_count {
            debug_assert!(tile_id < self.tile_count, "unadvertised tile id");
            return Push {
                ready: Vec::new(),
                dropped: 0,
            };
        }

        let partial = self.pending.entry(seq).or_insert_with(|| Partial {
            tiles: std::iter::repeat_with(|| None)
                .take(self.tile_count)
                .collect(),
        });
        // An encoder should produce one AU for one submitted tile. Treat a repeated
        // callback as idempotent rather than letting it complete or count twice.
        if partial.tiles[tile_id].is_some() {
            return Push {
                ready: Vec::new(),
                dropped: 0,
            };
        }
        partial.tiles[tile_id] = Some(tile);

        let mut dropped = 0;
        while self.pending.len() > self.max_pending {
            let oldest = *self
                .pending
                .first_key_value()
                .expect("length proved a pending frame")
                .0;
            self.pending.remove(&oldest);
            self.retire(oldest);
            dropped += 1;
        }

        let mut ready = Vec::new();
        loop {
            let Some((&oldest, partial)) = self.pending.first_key_value() else {
                break;
            };
            if partial.tiles.iter().any(Option::is_none) {
                break;
            }
            let partial = self
                .pending
                .remove(&oldest)
                .expect("oldest pending frame still exists");
            let tiles = partial
                .tiles
                .into_iter()
                .map(|tile| tile.expect("completeness checked above"))
                .collect();
            self.retire(oldest);
            ready.push(Complete { seq: oldest, tiles });
        }

        Push { ready, dropped }
    }

    pub(crate) fn discard_through(&mut self, seq: u64) {
        self.pending.clear();
        self.retire(seq);
    }

    fn retire(&mut self, seq: u64) {
        self.retired_through = Some(self.retired_through.map_or(seq, |old| old.max(seq)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_tiles_complete_once_in_tile_order() {
        let mut a = Assembler::new(2, 3);
        assert!(a.push(7, 1, "right").ready.is_empty());
        let result = a.push(7, 0, "left");
        assert_eq!(result.dropped, 0);
        assert_eq!(result.ready.len(), 1);
        assert_eq!(result.ready[0].seq, 7);
        assert_eq!(result.ready[0].tiles, ["left", "right"]);
        assert!(a.push(7, 1, "duplicate").ready.is_empty());
    }

    #[test]
    fn one_tile_completes_immediately() {
        let mut a = Assembler::new(1, 2);
        let result = a.push(3, 0, 99);
        assert_eq!(result.ready[0].tiles, [99]);
    }

    #[test]
    fn waits_for_older_sequence_then_releases_in_order() {
        let mut a = Assembler::new(2, 3);
        assert!(a.push(10, 0, "10-left").ready.is_empty());
        assert!(a.push(11, 0, "11-left").ready.is_empty());
        assert!(a.push(11, 1, "11-right").ready.is_empty());
        let result = a.push(10, 1, "10-right");
        assert_eq!(
            result
                .ready
                .iter()
                .map(|frame| frame.seq)
                .collect::<Vec<_>>(),
            [10, 11]
        );
    }

    #[test]
    fn overflow_drops_oldest_once_and_late_tile_cannot_resurrect_it() {
        let mut a = Assembler::new(2, 2);
        assert!(a.push(20, 0, "20-left").ready.is_empty());
        assert!(a.push(21, 0, "21-left").ready.is_empty());
        let overflow = a.push(22, 0, "22-left");
        assert_eq!(overflow.dropped, 1);
        assert!(a.push(20, 1, "20-late").ready.is_empty());
        let result = a.push(21, 1, "21-right");
        assert_eq!(result.ready.len(), 1);
        assert_eq!(result.ready[0].seq, 21);
    }

    #[test]
    fn recovery_suppresses_deltas_and_survives_a_full_queue() {
        let mut recovery = Recovery::waiting();
        assert!(!recovery.allows_overlays());
        recovery.drop_incomplete(1);
        assert_eq!(recovery.prepare(false, false), RecoveryDecision::Suppress);
        assert_eq!(recovery.dropped(), 2);
        assert_eq!(recovery.prepare(true, true), RecoveryDecision::Admit);
        recovery.queue_full();
        assert_eq!(recovery.dropped(), 3);
        assert_eq!(
            recovery.prepare(true, false),
            RecoveryDecision::SuppressAndRequest
        );
        assert_eq!(recovery.prepare(true, true), RecoveryDecision::Admit);
        recovery.admitted(true);
        assert!(recovery.allows_overlays());
        assert!(!recovery.waiting_for_keyframe());
        let dropped = recovery.dropped();
        recovery.drop_before_encode();
        assert_eq!(recovery.dropped(), dropped + 1);
        assert!(!recovery.waiting_for_keyframe());
        recovery.reset();
        assert!(!recovery.allows_overlays());
        assert!(recovery.waiting_for_keyframe());
    }

    #[test]
    fn reconnect_discards_old_parts_without_blocking_new_frames() {
        let mut a = Assembler::new(2, 3);
        assert!(a.push(30, 0, "old-left").ready.is_empty());
        a.discard_through(30);
        assert!(a.push(30, 1, "old-right").ready.is_empty());
        assert!(a.push(31, 0, "new-left").ready.is_empty());
        let result = a.push(31, 1, "new-right");
        assert_eq!(result.ready.len(), 1);
        assert_eq!(result.ready[0].seq, 31);
    }
}
