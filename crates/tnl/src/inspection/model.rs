use serde::Serialize;
pub use tnl_inspector_api::inspection::{
    BodyPreview, BodyState, CaptureId, GenerationState, Header, HeadersState, Message, Origin,
    PreviewMode, RequestBoundary, ResponseSource, State, Summary, TunnelGeneration, TunnelStatus,
    Visitor,
};
pub(super) use tnl_inspector_api::ReplayDisplay;

/// Sanitized wire data with runtime-only capture ownership kept outside its DTO.
#[derive(Clone, Debug, Serialize)]
pub struct InspectionRecord {
    #[serde(flatten)]
    pub(crate) view: tnl_inspector_api::inspection::InspectionRecord,
    #[serde(skip)]
    pub(crate) started: std::time::Instant,
    #[serde(skip)]
    pub(crate) terminal: bool,
    #[serde(skip)]
    pub(crate) counters: Option<TransportCounters>,
}
impl std::ops::Deref for InspectionRecord {
    type Target = tnl_inspector_api::inspection::InspectionRecord;
    fn deref(&self) -> &Self::Target {
        &self.view
    }
}
impl std::ops::DerefMut for InspectionRecord {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.view
    }
}
impl InspectionRecord {
    pub(crate) fn snapshot(&self) -> Self {
        let mut record = self.clone();
        if !record.terminal {
            record.duration_ms = elapsed_ms(record.started);
        }
        if let Some(counters) = &record.counters {
            use std::sync::atomic::Ordering;
            {
                let timing = counters.timing.lock();
                record.view.timing = timing.snapshot_at(super::timing::offset_us(record.started));
            }
            let to_backend = counters.to_backend.load(Ordering::Relaxed);
            let to_client = counters.to_client.load(Ordering::Relaxed);
            record.bytes_to_backend = to_backend;
            record.bytes_to_client = to_client;
            record.bytes_from_backend = if record.response_source == ResponseSource::Backend {
                record.bytes_to_client
            } else {
                0
            };
        }
        record
    }
    pub(crate) fn omit(&mut self, reason: &'static str) {
        let reason = match reason {
            "head-limit" | "chunk-limit" | "trailer-limit" | "informational-limit" => {
                "capture-limit"
            }
            "malformed-head" | "ambiguous-framing" | "invalid-chunk" | "invalid-trailer"
            | "unexpected-data" => "invalid-framing",
            "cancelled" => "incomplete",
            other => other,
        };
        if self.omission_reasons.len() < 16 && !self.omission_reasons.contains(&reason) {
            self.omission_reasons.push(reason);
        }
    }
}
pub(super) fn elapsed_ms(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}
pub fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[derive(Clone, Debug)]
pub struct TransportCounters {
    pub timing: std::sync::Arc<parking_lot::Mutex<super::timing::TimingState>>,
    pub to_backend: std::sync::Arc<std::sync::atomic::AtomicU64>,
    pub to_client: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

pub(super) struct Detail {
    pub record: InspectionRecord,
    pub replay: ReplayDisplay,
    pub last_replay_outcome: Option<crate::replay::ReplayOutcome>,
}
impl Serialize for Detail {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        tnl_inspector_api::DetailReply {
            record: &self.record.view,
            replay: &self.replay,
            last_replay_outcome: &self.last_replay_outcome,
        }
        .serialize(serializer)
    }
}

#[cfg(test)]
impl InspectionRecord {
    pub(crate) fn allocated_capacity(&self) -> usize {
        let mut bytes = std::mem::size_of::<Self>()
            + self.tunnel.capacity()
            + self.display_target.capacity()
            + self.method.capacity()
            + self.request_target.capacity()
            + self.omission_reasons.capacity() * std::mem::size_of::<&str>();
        for message in [&self.request, &self.response] {
            bytes += message.headers.capacity() * std::mem::size_of::<Header>();
            bytes += message
                .headers
                .iter()
                .map(|h| h.name.capacity() + h.value.capacity())
                .sum::<usize>();
            bytes += message.body.text.as_ref().map_or(0, String::capacity);
        }
        if self.counters.is_some() {
            bytes += std::mem::size_of::<parking_lot::Mutex<super::timing::TimingState>>()
                + 2 * std::mem::size_of::<usize>();
            bytes += 2
                * (std::mem::size_of::<std::sync::atomic::AtomicU64>()
                    + 2 * std::mem::size_of::<usize>());
        }
        bytes
    }
}
