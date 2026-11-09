use tnl_inspector_api::timing::{
    Interval, Intervals, Outcome, Reason, Snapshot, Timing, MAX_OFFSET_US,
};
const fn snapshot() -> Snapshot {
    let none = Interval::not_started();
    Snapshot {
        as_of_offset_us: 0,
        terminal: false,
        intervals: Intervals {
            metadata: none,
            request_head: none,
            resolve: none,
            connect: none,
            request_write: none,
            response_read: none,
            response_write: none,
            upgraded: none,
        },
        backend_first_byte_offset_us: None,
        backend_final_headers_offset_us: None,
        request_framing_end_offset_us: None,
        response_framing_end_offset_us: None,
        request_framing_valid: false,
        response_framing_valid: false,
    }
}
#[test]
fn timing_constructors_preserve_observed_zero_and_reject_invalid_measurements() {
    let valid = snapshot();
    assert!(valid.is_valid());
    assert_eq!(Timing::from_snapshot(valid).snapshot(), Some(valid));
    assert_eq!(Timing::unavailable(Reason::NotObserved).snapshot(), None);
    let mut bad = valid;
    bad.as_of_offset_us = MAX_OFFSET_US + 1;
    assert!(!bad.is_valid());
    assert_eq!(
        Timing::from_snapshot(bad).reason(),
        Some(Reason::InvalidMeasurement)
    );
    for interval in [
        Interval {
            outcome: Outcome::Completed,
            start_offset_us: Some(1),
            end_offset_us: Some(0),
        },
        Interval {
            outcome: Outcome::Completed,
            start_offset_us: Some(0),
            end_offset_us: None,
        },
        Interval {
            outcome: Outcome::Skipped,
            start_offset_us: Some(0),
            end_offset_us: Some(0),
        },
        Interval {
            outcome: Outcome::Active,
            start_offset_us: Some(0),
            end_offset_us: Some(0),
        },
    ] {
        bad = valid;
        bad.intervals.connect = interval;
        assert!(!bad.is_valid(), "{interval:?}");
    }
    bad = valid;
    bad.terminal = true;
    bad.intervals.connect = Interval {
        outcome: Outcome::Active,
        start_offset_us: Some(0),
        end_offset_us: None,
    };
    assert!(!bad.is_valid());
    bad = valid;
    bad.request_framing_valid = true;
    assert!(!bad.is_valid());
    bad = valid;
    bad.backend_final_headers_offset_us = Some(0);
    assert!(!bad.is_valid());
}
