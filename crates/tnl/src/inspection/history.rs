use super::{
    Capture, CaptureContext, CaptureId, ConsumerFactory, InspectionRecord, Message, PreviewMode,
    State, TunnelGeneration,
};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tnl_protocol::request_metadata::RequestMetadataV1;

pub(super) const MAX_RECORDS: usize = 200;
// Conservative reservation covers bounded strings, headers, previews and model
// allocations, not merely serialized lengths. 200 * 64 KiB < 16 MiB.
const RECORD_RESERVATION: usize = 64 * 1024;
const HISTORY_BYTES: usize = 16 * 1024 * 1024;
const TTL: Duration = Duration::from_secs(15 * 60);
struct Row {
    admitted: Instant,
    record: InspectionRecord,
}
pub(super) struct History {
    rows: VecDeque<Row>,
    next: u64,
}
impl History {
    fn sweep(&mut self, now: Instant) {
        while self
            .rows
            .front()
            .is_some_and(|r| now.saturating_duration_since(r.admitted) >= TTL)
        {
            self.rows.pop_front();
        }
    }
    fn insert(&mut self, mut record: InspectionRecord, now: Instant) {
        self.sweep(now);
        while self.rows.len() >= MAX_RECORDS
            || (self.rows.len() + 1) * RECORD_RESERVATION > HISTORY_BYTES
        {
            self.rows.pop_front();
        }
        self.next = self.next.saturating_add(1);
        record.sequence = self.next;
        self.rows.push_back(Row {
            admitted: now,
            record,
        });
    }
}
/// All stored/searchable fields are sanitized before mutation. Payload-bearing
/// observer scratch is deliberately not Debug or Serialize.
#[derive(Clone)]
pub struct Inspection {
    pub(super) inner: Arc<Inner>,
}
pub(super) struct Inner {
    pub(super) history: Mutex<History>,
    pub(super) active: AtomicUsize,
    pub(super) mode: PreviewMode,
    pub(super) consumer: Option<Arc<dyn ConsumerFactory>>,
}
impl std::fmt::Debug for Inspection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inspection")
            .field("active", &self.inner.active.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}
impl Inspection {
    pub fn new(mode: PreviewMode) -> Self {
        Self::with_consumer(mode, None)
    }
    /// Consumer factories must perform their own bounded, synchronous admission
    /// and may decline independently. No events are enqueued by inspection.
    pub fn with_consumer(mode: PreviewMode, consumer: Option<Arc<dyn ConsumerFactory>>) -> Self {
        Self {
            inner: Arc::new(Inner {
                history: Mutex::new(History {
                    rows: VecDeque::with_capacity(MAX_RECORDS),
                    next: 0,
                }),
                active: AtomicUsize::new(0),
                mode,
                consumer,
            }),
        }
    }
    pub fn admit(
        &self,
        generation: TunnelGeneration,
        metadata: Option<&RequestMetadataV1>,
    ) -> Option<Capture> {
        self.inner
            .active
            .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |n| {
                (n < 64).then_some(n + 1)
            })
            .ok()?;
        let context = CaptureContext {
            capture_id: CaptureId(ulid::Ulid::new()),
            generation,
            request_id: metadata.map(|m| m.request_id),
            backend: None,
        };
        let record = InspectionRecord {
            capture_id: context.capture_id,
            generation,
            request_id: context.request_id,
            visitor_source: metadata.map(|m| m.visitor.source.as_str()),
            sequence: 0,
            method: String::new(),
            request_target: String::new(),
            status: None,
            state: State::Active,
            request: Message::new(self.inner.mode),
            response: Message::new(self.inner.mode),
            observation_incomplete: false,
        };
        self.inner.history.lock().insert(record, Instant::now());
        Some(Capture::new(self.clone(), context))
    }
    pub(super) fn update(&self, id: CaptureId, update: impl FnOnce(&mut InspectionRecord)) {
        let mut history = self.inner.history.lock();
        history.sweep(Instant::now());
        if let Some(row) = history.rows.iter_mut().find(|r| r.record.capture_id == id) {
            update(&mut row.record);
        }
    }
    pub fn get(&self, id: CaptureId) -> Option<InspectionRecord> {
        let mut history = self.inner.history.lock();
        history.sweep(Instant::now());
        history
            .rows
            .iter()
            .find(|r| r.record.capture_id == id)
            .map(|r| r.record.clone())
    }
    /// Literal, bounded search of sanitized fields only; at most 100 results.
    pub fn search(&self, query: &str) -> Vec<InspectionRecord> {
        if query.len() > 256 {
            return Vec::new();
        }
        let query = query.to_lowercase();
        let mut history = self.inner.history.lock();
        history.sweep(Instant::now());
        history
            .rows
            .iter()
            .rev()
            .filter(|row| {
                let r = &row.record;
                r.method.to_lowercase().contains(&query)
                    || r.request_target.to_lowercase().contains(&query)
                    || [&r.request, &r.response].iter().any(|m| {
                        m.headers.iter().any(|h| {
                            h.name.to_lowercase().contains(&query)
                                || h.value.to_lowercase().contains(&query)
                        }) || m
                            .body
                            .text
                            .as_ref()
                            .is_some_and(|s| s.to_lowercase().contains(&query))
                    })
            })
            .take(100)
            .map(|r| r.record.clone())
            .collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn count_bytes_expiry_and_no_resurrection() {
        let inspection = Inspection::new(PreviewMode::Off);
        let first = inspection.admit(TunnelGeneration::new(), None).unwrap();
        let id = first.id();
        for _ in 0..MAX_RECORDS {
            drop(inspection.admit(TunnelGeneration::new(), None).unwrap());
        }
        assert!(inspection.get(id).is_none());
        drop(first);
        assert!(inspection.get(id).is_none());
        let mut h = inspection.inner.history.lock();
        assert_eq!(h.rows.len(), 200);
        assert!(h.rows.len() * RECORD_RESERVATION <= HISTORY_BYTES);
        h.sweep(Instant::now() + TTL);
        assert!(h.rows.is_empty());
        drop(h);
    }
}
