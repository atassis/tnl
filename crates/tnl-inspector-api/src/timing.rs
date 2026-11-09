use schemars::JsonSchema;
use serde::Serialize;

pub const MAX_OFFSET_US: u64 = 9_007_199_254_740_991;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    NotStarted,
    Skipped,
    Active,
    Completed,
    Failed,
    Cancelled,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Interval {
    pub outcome: Outcome,
    #[schemars(range(min = 0, max = 9_007_199_254_740_991_u64))]
    pub start_offset_us: Option<u64>,
    #[schemars(range(min = 0, max = 9_007_199_254_740_991_u64))]
    pub end_offset_us: Option<u64>,
}
impl Interval {
    pub const fn not_started() -> Self {
        Self {
            outcome: Outcome::NotStarted,
            start_offset_us: None,
            end_offset_us: None,
        }
    }
    const fn valid(self, as_of: u64, terminal: bool) -> bool {
        match (self.outcome, self.start_offset_us, self.end_offset_us) {
            (Outcome::NotStarted | Outcome::Skipped | Outcome::Unavailable, None, None) => true,
            (Outcome::Active, Some(start), None) => !terminal && start <= as_of,
            (Outcome::Completed | Outcome::Failed | Outcome::Cancelled, Some(start), Some(end)) => {
                start <= end && end <= as_of
            }
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Intervals {
    pub metadata: Interval,
    pub request_head: Interval,
    pub resolve: Interval,
    pub connect: Interval,
    pub request_write: Interval,
    pub response_read: Interval,
    pub response_write: Interval,
    pub upgraded: Interval,
}
impl Intervals {
    pub const fn as_array(self) -> [Interval; 8] {
        [
            self.metadata,
            self.request_head,
            self.resolve,
            self.connect,
            self.request_write,
            self.response_read,
            self.response_write,
            self.upgraded,
        ]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Snapshot {
    #[schemars(range(min = 0, max = 9_007_199_254_740_991_u64))]
    pub as_of_offset_us: u64,
    pub terminal: bool,
    pub intervals: Intervals,
    #[schemars(range(min = 0, max = 9_007_199_254_740_991_u64))]
    pub backend_first_byte_offset_us: Option<u64>,
    #[schemars(range(min = 0, max = 9_007_199_254_740_991_u64))]
    pub backend_final_headers_offset_us: Option<u64>,
    #[schemars(range(min = 0, max = 9_007_199_254_740_991_u64))]
    pub request_framing_end_offset_us: Option<u64>,
    #[schemars(range(min = 0, max = 9_007_199_254_740_991_u64))]
    pub response_framing_end_offset_us: Option<u64>,
    pub request_framing_valid: bool,
    pub response_framing_valid: bool,
}
impl Snapshot {
    pub fn is_valid(self) -> bool {
        if self.as_of_offset_us > MAX_OFFSET_US
            || !self
                .intervals
                .as_array()
                .into_iter()
                .all(|span| span.valid(self.as_of_offset_us, self.terminal))
        {
            return false;
        }
        let milestones = [
            self.backend_first_byte_offset_us,
            self.backend_final_headers_offset_us,
            self.request_framing_end_offset_us,
            self.response_framing_end_offset_us,
        ];
        milestones
            .into_iter()
            .flatten()
            .all(|offset| offset <= self.as_of_offset_us)
            && (!self.request_framing_valid || self.request_framing_end_offset_us.is_some())
            && (!self.response_framing_valid || self.response_framing_end_offset_us.is_some())
            && self.backend_final_headers_offset_us.is_none_or(|end| {
                self.backend_first_byte_offset_us
                    .is_some_and(|start| start <= end)
            })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    NotObserved,
    ReplayNotInstrumented,
    ClockOverflow,
    InvalidMeasurement,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Scope {
    ClientBackend,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Availability {
    Available,
    Unavailable,
}

/// Constructors keep absence distinct from an observed zero-length activity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Timing {
    #[schemars(extend("const" = 1))]
    schema_version: u8,
    scope: Scope,
    availability: Availability,
    reason: Option<Reason>,
    snapshot: Option<Snapshot>,
}
impl Timing {
    pub const fn unavailable(reason: Reason) -> Self {
        Self {
            schema_version: 1,
            scope: Scope::ClientBackend,
            availability: Availability::Unavailable,
            reason: Some(reason),
            snapshot: None,
        }
    }
    pub fn from_snapshot(snapshot: Snapshot) -> Self {
        if !snapshot.is_valid() {
            return Self::unavailable(Reason::InvalidMeasurement);
        }
        Self {
            schema_version: 1,
            scope: Scope::ClientBackend,
            availability: Availability::Available,
            reason: None,
            snapshot: Some(snapshot),
        }
    }
    pub const fn snapshot(self) -> Option<Snapshot> {
        self.snapshot
    }
    pub const fn reason(self) -> Option<Reason> {
        self.reason
    }
}
