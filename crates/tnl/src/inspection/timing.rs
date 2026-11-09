use super::Direction;
pub use tnl_inspector_api::timing::Outcome;
use tnl_inspector_api::timing::{Interval, Intervals, Reason, Snapshot, Timing};

#[derive(Clone, Copy, Debug)]
pub enum Activity {
    Metadata,
    RequestHead,
    Resolve,
    Connect,
    RequestWrite,
    ResponseRead,
    ResponseWrite,
    Upgraded,
}
#[derive(Clone, Copy, Debug)]
pub enum Milestone {
    BackendFirstByte,
    BackendFinalHeaders,
    RequestFramingEnd,
    ResponseFramingEnd,
}

#[derive(Clone, Copy, Debug)]
pub struct TimingState {
    intervals: [Interval; 8],
    milestones: [Option<u64>; 4],
    framing_revoked: [bool; 2],
    last_offset: u64,
    terminal_at: Option<u64>,
    invalid: Option<Reason>,
    accepted: [u64; 2],
    last_write_at: [Option<u64>; 2],
    write_ends: [Option<u64>; 2],
    upgraded: bool,
    synthetic: bool,
}
impl TimingState {
    pub const fn new() -> Self {
        Self {
            intervals: [Interval::not_started(); 8],
            milestones: [None; 4],
            framing_revoked: [false; 2],
            last_offset: 0,
            terminal_at: None,
            invalid: None,
            accepted: [0; 2],
            last_write_at: [None; 2],
            write_ends: [None; 2],
            upgraded: false,
            synthetic: false,
        }
    }
    const fn observe_offset(&mut self, at: u64) -> bool {
        if self.terminal_at.is_some() || self.invalid.is_some() {
            return false;
        }
        if at > tnl_inspector_api::timing::MAX_OFFSET_US {
            self.invalid = Some(Reason::ClockOverflow);
        } else if at < self.last_offset {
            self.invalid = Some(Reason::InvalidMeasurement);
        } else {
            self.last_offset = at;
            return true;
        }
        false
    }
    pub fn start_at(&mut self, activity: Activity, at: u64) {
        if !self.observe_offset(at) {
            return;
        }
        let interval = &mut self.intervals[activity as usize];
        if interval.outcome == Outcome::NotStarted {
            interval.outcome = Outcome::Active;
            interval.start_offset_us = Some(at);
        }
    }
    pub const fn finish_at(&mut self, activity: Activity, outcome: Outcome, at: u64) {
        if !self.observe_offset(at) {
            return;
        }
        let interval = &mut self.intervals[activity as usize];
        match (interval.outcome, outcome) {
            (Outcome::NotStarted, Outcome::Skipped | Outcome::Unavailable) => {
                interval.outcome = outcome;
            }
            (Outcome::Active, Outcome::Completed | Outcome::Failed | Outcome::Cancelled) => {
                interval.outcome = outcome;
                interval.end_offset_us = Some(at);
            }
            _ => {}
        }
    }
    pub fn milestone_at(&mut self, milestone: Milestone, at: u64) {
        if self.observe_offset(at) {
            self.milestones[milestone as usize].get_or_insert(at);
        }
    }
    pub const fn cancel_framing(&mut self, direction: Direction) {
        if self.terminal_at.is_none() {
            self.framing_revoked[direction_index(direction)] = true;
        }
    }
    pub fn revoke_framing(&mut self, direction: Direction) {
        if self.terminal_at.is_none() {
            let index = direction_index(direction);
            self.framing_revoked[index] = true;
            let interval = &mut self.intervals[write_activity(direction) as usize];
            if interval.outcome == Outcome::Completed {
                interval.outcome = Outcome::Failed;
            }
        }
    }
    // A provisional framing end completes writes only after accepted local
    // bytes cross its wire watermark. This never claims remote delivery.
    pub fn accepted_at(&mut self, direction: Direction, bytes: u64, at: u64) {
        if !self.observe_offset(at) {
            return;
        }
        let index = direction_index(direction);
        if bytes == 0 {
            return;
        }
        self.last_write_at[index] = Some(at);
        self.accepted[index] = self.accepted[index].saturating_add(bytes);
        if self.writes_complete(direction) {
            self.finish_at(write_activity(direction), Outcome::Completed, at);
        }
    }
    fn writes_complete(&self, direction: Direction) -> bool {
        let index = direction_index(direction);
        self.write_ends[index].is_some_and(|end| self.accepted[index] >= end)
            && ((self.milestones[2 + index].is_some() && !self.framing_revoked[index])
                || (index == 1 && self.synthetic))
    }
    pub fn expect_write_end_at(&mut self, direction: Direction, bytes: u64, at: u64) {
        if self.observe_offset(at) {
            let index = direction_index(direction);
            self.write_ends[index].get_or_insert(bytes);
            if self.writes_complete(direction) {
                let interval = &mut self.intervals[write_activity(direction) as usize];
                if interval.outcome == Outcome::Active {
                    interval.outcome = Outcome::Completed;
                    interval.end_offset_us = self.last_write_at[index];
                }
            }
        }
    }
    pub fn upgrade_at(&mut self, head_bytes: u64, at: u64) {
        if !self.observe_offset(at) || self.upgraded {
            return;
        }
        self.upgraded = true;
        self.expect_write_end_at(Direction::Response, head_bytes, at);
        self.finish_at(Activity::ResponseRead, Outcome::Completed, at);
        self.start_at(Activity::Upgraded, at);
    }
    pub const fn synth_at(&mut self, bytes: u64, at: u64) {
        if self.observe_offset(at) {
            self.synthetic = true;
            self.write_ends[1] = Some(bytes);
        }
    }
    pub fn write_shutdown_at(&mut self, direction: Direction, at: u64) {
        if self.writes_complete(direction) {
            self.finish_at(write_activity(direction), Outcome::Completed, at);
        }
    }
    pub fn io_failure_at(&mut self, activity: Activity, at: u64) {
        if !self.observe_offset(at) {
            return;
        }
        let interval = &mut self.intervals[activity as usize];
        if interval.outcome == Outcome::Active {
            interval.outcome = Outcome::Failed;
            interval.end_offset_us = Some(at);
        }
    }
    pub const fn is_upgraded(&self) -> bool {
        self.upgraded
    }
    pub fn finalize_at(&mut self, at: u64) {
        if !self.observe_offset(at) {
            return;
        }
        for interval in &mut self.intervals {
            if interval.outcome == Outcome::Active {
                interval.outcome = Outcome::Cancelled;
                interval.end_offset_us = Some(at);
            }
        }
        self.terminal_at = Some(at);
    }
    pub fn snapshot_at(&self, at: u64) -> Timing {
        if let Some(reason) = self.invalid {
            return Timing::unavailable(reason);
        }
        let at = self.terminal_at.unwrap_or(at);
        if at > tnl_inspector_api::timing::MAX_OFFSET_US {
            return Timing::unavailable(Reason::ClockOverflow);
        }
        if at < self.last_offset {
            return Timing::unavailable(Reason::InvalidMeasurement);
        }
        let [metadata, request_head, resolve, connect, request_write, response_read, response_write, upgraded] =
            self.intervals;
        let [backend_first_byte_offset_us, backend_final_headers_offset_us, request_framing_end_offset_us, response_framing_end_offset_us] =
            self.milestones;
        Timing::from_snapshot(Snapshot {
            as_of_offset_us: at,
            terminal: self.terminal_at.is_some(),
            intervals: Intervals {
                metadata,
                request_head,
                resolve,
                connect,
                request_write,
                response_read,
                response_write,
                upgraded,
            },
            backend_first_byte_offset_us,
            backend_final_headers_offset_us,
            request_framing_end_offset_us,
            response_framing_end_offset_us,
            request_framing_valid: request_framing_end_offset_us.is_some()
                && !self.framing_revoked[0],
            response_framing_valid: response_framing_end_offset_us.is_some()
                && !self.framing_revoked[1],
        })
    }
}

const fn direction_index(direction: Direction) -> usize {
    match direction {
        Direction::Request => 0,
        Direction::Response => 1,
    }
}
const fn write_activity(direction: Direction) -> Activity {
    match direction {
        Direction::Request => Activity::RequestWrite,
        Direction::Response => Activity::ResponseWrite,
    }
}

pub fn offset_us(origin: std::time::Instant) -> u64 {
    u64::try_from(origin.elapsed().as_micros())
        .unwrap_or(tnl_inspector_api::timing::MAX_OFFSET_US + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tnl_inspector_api::timing::MAX_OFFSET_US;
    #[test]
    fn timing_distinguishes_skipped_resolution_from_observed_zero() {
        let mut t = TimingState::new();
        t.finish_at(Activity::Resolve, Outcome::Skipped, 0);
        t.start_at(Activity::Connect, 0);
        t.finish_at(Activity::Connect, Outcome::Completed, 0);
        let s = t.snapshot_at(0).snapshot().unwrap();
        assert_eq!(s.intervals.resolve.outcome, Outcome::Skipped);
        assert_eq!(s.intervals.resolve.start_offset_us, None);
        assert_eq!(s.intervals.connect.start_offset_us, Some(0));
        assert_eq!(s.intervals.connect.end_offset_us, Some(0));
    }
    #[test]
    fn timing_early_first_byte_overlaps_an_unfinished_upload() {
        let mut t = TimingState::new();
        t.start_at(Activity::RequestWrite, 10);
        t.start_at(Activity::ResponseRead, 11);
        t.milestone_at(Milestone::BackendFirstByte, 15);
        t.milestone_at(Milestone::BackendFinalHeaders, 17);
        t.start_at(Activity::ResponseWrite, 18);
        let s = t.snapshot_at(20).snapshot().unwrap();
        assert_eq!(s.intervals.request_write.outcome, Outcome::Active);
        assert_eq!(s.intervals.request_write.end_offset_us, None);
        assert_eq!(s.backend_first_byte_offset_us, Some(15));
        assert_eq!(s.backend_final_headers_offset_us, Some(17));
        assert_eq!(s.intervals.response_write.outcome, Outcome::Active);
    }
    #[test]
    fn timing_io_failure_does_not_rewrite_completed_write() {
        let mut t = TimingState::new();
        t.start_at(Activity::RequestWrite, 1);
        t.finish_at(Activity::RequestWrite, Outcome::Completed, 2);
        t.io_failure_at(Activity::RequestWrite, 3);
        let write = t.snapshot_at(4).snapshot().unwrap().intervals.request_write;
        assert_eq!(write.outcome, Outcome::Completed);
        assert_eq!(write.end_offset_us, Some(2));
    }
    #[test]
    fn timing_finalization_is_once_and_preserves_completed_resolve_after_connect_failure() {
        let mut t = TimingState::new();
        t.start_at(Activity::Resolve, 0);
        t.finish_at(Activity::Resolve, Outcome::Completed, 5);
        t.start_at(Activity::Connect, 5);
        t.finish_at(Activity::Connect, Outcome::Failed, 9);
        t.finalize_at(10);
        let before = t.snapshot_at(100);
        t.finalize_at(200);
        t.start_at(Activity::RequestWrite, 201);
        assert_eq!(before, t.snapshot_at(300));
        let s = before.snapshot().unwrap();
        assert!(s.terminal);
        assert_eq!(s.as_of_offset_us, 10);
        assert_eq!(s.intervals.resolve.outcome, Outcome::Completed);
        assert_eq!(s.intervals.connect.outcome, Outcome::Failed);
        assert_eq!(s.intervals.request_write.outcome, Outcome::NotStarted);
    }
    #[test]
    fn timing_cancels_only_unfinished_activities() {
        let mut t = TimingState::new();
        t.start_at(Activity::Metadata, 0);
        t.finish_at(Activity::Metadata, Outcome::Completed, 1);
        t.start_at(Activity::RequestHead, 1);
        t.finalize_at(3);
        let s = t.snapshot_at(8).snapshot().unwrap();
        assert_eq!(s.intervals.metadata.outcome, Outcome::Completed);
        assert_eq!(s.intervals.request_head.outcome, Outcome::Cancelled);
        assert_eq!(s.intervals.request_head.end_offset_us, Some(3));
        assert_eq!(s.intervals.upgraded.outcome, Outcome::NotStarted);
    }
    #[test]
    fn timing_cancellation_transitions_only_active_io_phases() {
        let mut t = TimingState::new();
        for (activity, start, end) in [
            (Activity::Metadata, 0, 1),
            (Activity::RequestHead, 2, 3),
            (Activity::Resolve, 4, 5),
            (Activity::Connect, 6, 7),
        ] {
            t.start_at(activity, start);
            t.finish_at(activity, Outcome::Completed, end);
        }
        for activity in [
            Activity::RequestWrite,
            Activity::ResponseRead,
            Activity::ResponseWrite,
            Activity::Upgraded,
        ] {
            t.start_at(activity, 8);
        }
        t.finalize_at(9);
        let intervals = t.snapshot_at(5).snapshot().unwrap().intervals;
        for (interval, end) in [
            (intervals.metadata, 1),
            (intervals.request_head, 3),
            (intervals.resolve, 5),
            (intervals.connect, 7),
        ] {
            assert_eq!(interval.outcome, Outcome::Completed);
            assert_eq!(interval.end_offset_us, Some(end));
        }
        for interval in [
            intervals.request_write,
            intervals.response_read,
            intervals.response_write,
            intervals.upgraded,
        ] {
            assert_eq!(interval.outcome, Outcome::Cancelled);
            assert_eq!(interval.end_offset_us, Some(9));
        }
    }
    #[test]
    fn timing_revocation_is_sticky_without_inventing_a_new_framing_end() {
        let mut t = TimingState::new();
        t.milestone_at(Milestone::RequestFramingEnd, 2);
        t.milestone_at(Milestone::BackendFirstByte, 3);
        t.milestone_at(Milestone::BackendFinalHeaders, 4);
        t.milestone_at(Milestone::ResponseFramingEnd, 5);
        t.revoke_framing(Direction::Request);
        t.revoke_framing(Direction::Response);
        t.milestone_at(Milestone::RequestFramingEnd, 6);
        t.milestone_at(Milestone::ResponseFramingEnd, 7);
        let s = t.snapshot_at(9).snapshot().unwrap();
        assert!(!s.request_framing_valid);
        assert!(!s.response_framing_valid);
        assert_eq!(s.request_framing_end_offset_us, Some(2));
        assert_eq!(s.response_framing_end_offset_us, Some(5));
    }
    #[test]
    fn timing_overflow_and_nonmonotonic_measurements_are_unavailable() {
        let mut t = TimingState::new();
        t.start_at(Activity::Upgraded, MAX_OFFSET_US + 1);
        assert_eq!(
            t.snapshot_at(MAX_OFFSET_US + 1).reason(),
            Some(Reason::ClockOverflow)
        );
        let mut t = TimingState::new();
        t.start_at(Activity::RequestHead, 5);
        t.finish_at(Activity::RequestHead, Outcome::Completed, 4);
        assert_eq!(t.snapshot_at(8).reason(), Some(Reason::InvalidMeasurement));
        assert_eq!(
            TimingState::new().snapshot_at(MAX_OFFSET_US + 1).reason(),
            Some(Reason::ClockOverflow)
        );
    }
    #[test]
    fn timing_write_completion_requires_framing_and_actual_accepted_bytes() {
        let mut t = TimingState::new();
        t.milestone_at(Milestone::RequestFramingEnd, 1);
        t.expect_write_end_at(Direction::Request, 10, 1);
        t.start_at(Activity::RequestWrite, 2);
        t.accepted_at(Direction::Request, 3, 3);
        assert_eq!(
            t.snapshot_at(3)
                .snapshot()
                .unwrap()
                .intervals
                .request_write
                .outcome,
            Outcome::Active
        );
        t.accepted_at(Direction::Request, 7, 4);
        let s = t.snapshot_at(4).snapshot().unwrap();
        assert_eq!(s.intervals.request_write.outcome, Outcome::Completed);
        assert_eq!(s.intervals.request_write.end_offset_us, Some(4));
        t.revoke_framing(Direction::Request);
        let s = t.snapshot_at(5).snapshot().unwrap();
        assert_eq!(s.intervals.request_write.outcome, Outcome::Failed);
        assert_eq!(s.request_framing_end_offset_us, Some(1));
        assert!(!s.request_framing_valid);
    }
    #[test]
    fn timing_upgrade_separates_backend_handshake_from_late_accepted_handshake_writes() {
        let mut t = TimingState::new();
        t.start_at(Activity::ResponseRead, 1);
        t.milestone_at(Milestone::BackendFirstByte, 2);
        t.start_at(Activity::ResponseWrite, 3);
        t.accepted_at(Direction::Response, 9, 3);
        t.milestone_at(Milestone::BackendFinalHeaders, 4);
        t.milestone_at(Milestone::ResponseFramingEnd, 4);
        t.upgrade_at(100, 4);
        let s = t.snapshot_at(5).snapshot().unwrap();
        assert!(t.is_upgraded());
        assert_eq!(s.intervals.response_read.outcome, Outcome::Completed);
        assert_eq!(s.intervals.response_write.outcome, Outcome::Active);
        assert_eq!(s.intervals.upgraded.outcome, Outcome::Active);
        t.accepted_at(Direction::Response, 500, 6);
        let s = t.snapshot_at(7).snapshot().unwrap();
        assert_eq!(s.intervals.response_write.outcome, Outcome::Completed);
        assert_eq!(s.intervals.response_write.end_offset_us, Some(6));
        assert_eq!(s.intervals.upgraded.outcome, Outcome::Active);
    }
    #[test]
    fn timing_synthetic_writes_never_manufacture_backend_milestones() {
        let mut t = TimingState::new();
        t.synth_at(100, 1);
        t.start_at(Activity::ResponseWrite, 2);
        t.accepted_at(Direction::Response, 100, 3);
        t.write_shutdown_at(Direction::Response, 4);
        let s = t.snapshot_at(5).snapshot().unwrap();
        assert_eq!(s.intervals.response_write.outcome, Outcome::Completed);
        assert_eq!(s.backend_first_byte_offset_us, None);
        assert_eq!(s.backend_final_headers_offset_us, None);
        assert_eq!(s.response_framing_end_offset_us, None);
    }
    #[test]
    fn timing_storage_is_fixed_and_has_no_heap_ownership() {
        assert!(!std::mem::needs_drop::<TimingState>());
        assert!(std::mem::size_of::<TimingState>() <= 512);
    }
}
