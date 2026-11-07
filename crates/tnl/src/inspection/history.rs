use super::{
    Capture, CaptureContext, CaptureId, ConsumerFactory, GenerationState, InspectionRecord,
    Message, PreviewMode, State, Summary, TunnelGeneration, TunnelStatus,
};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
    tunnel: TunnelStatus,
    tunnel_id: Option<ulid::Ulid>,
    replay: Option<crate::replay::ReplayController>,
}
impl History {
    // All dual-store operations take history -> replay. The controller never
    // calls history; no await or network operation runs under this lock.
    fn pop_front(&mut self) {
        if let Some(row) = self.rows.pop_front() {
            if let Some(replay) = &self.replay {
                replay.forget(row.record.capture_id);
            }
        }
    }
    fn sweep(&mut self, now: Instant) {
        while self
            .rows
            .front()
            .is_some_and(|r| now.saturating_duration_since(r.admitted) >= TTL)
        {
            self.pop_front();
        }
        if let Some(replay) = &self.replay {
            replay.sweep();
        }
    }
    fn insert(&mut self, mut record: InspectionRecord, now: Instant) {
        self.sweep(now);
        while self.rows.len() >= MAX_RECORDS
            || (self.rows.len() + 1) * RECORD_RESERVATION > HISTORY_BYTES
        {
            self.pop_front();
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
    closed: AtomicBool,
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
                    tunnel_id: None,
                    replay: None,
                    tunnel: TunnelStatus {
                        generation: None,
                        connected: false,
                    },
                }),
                active: AtomicUsize::new(0),
                mode,
                closed: AtomicBool::new(false),
                consumer,
            }),
        }
    }
    /// The same controller is both the single observer consumer and replay
    /// authority. Bind its forbidden inspector port before constructing it.
    pub fn with_replay(mode: PreviewMode, replay: crate::replay::ReplayController) -> Self {
        let inspection = Self::with_consumer(mode, Some(Arc::new(replay.clone())));
        inspection.inner.history.lock().replay = Some(replay);
        inspection
    }
    pub fn admit(
        &self,
        generation: TunnelGeneration,
        metadata: Option<&RequestMetadataV1>,
    ) -> Option<Capture> {
        self.admit_inner(generation, metadata, None, true)
    }
    pub fn admit_backend(
        &self,
        generation: TunnelGeneration,
        metadata: Option<&RequestMetadataV1>,
        backend: std::net::SocketAddr,
    ) -> Option<Capture> {
        self.admit_inner(generation, metadata, Some(backend), true)
    }
    pub(crate) fn admit_preconnect(
        &self,
        generation: TunnelGeneration,
        metadata: Option<&RequestMetadataV1>,
    ) -> Option<Capture> {
        self.admit_inner(generation, metadata, None, false)
    }
    fn admit_inner(
        &self,
        generation: TunnelGeneration,
        metadata: Option<&RequestMetadataV1>,
        backend: Option<std::net::SocketAddr>,
        raw_consumer: bool,
    ) -> Option<Capture> {
        if self.inner.closed.load(Ordering::Acquire) {
            return None;
        }
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
            backend,
        };
        let record = InspectionRecord {
            schema_version: 1,
            tunnel: String::new(),
            tunnel_id: None,
            origin: super::Origin::External,
            started_at_ms: super::model::wall_ms(),
            duration_ms: 0,
            started: Instant::now(),
            terminal: false,
            counters: None,
            failure_kind: None,
            display_target: String::new(),
            resolved_addr: None,
            response_source: super::ResponseSource::Unavailable,
            request_boundary: if raw_consumer {
                super::RequestBoundary::EffectiveBackend
            } else {
                super::RequestBoundary::PreconnectUnavailable
            },
            bytes_to_backend: 0,
            bytes_from_backend: 0,
            bytes_to_client: 0,
            byte_count_semantics: "successful_local_writes_not_delivery",
            omission_reasons: Vec::new(),
            visitor: super::Visitor {
                available: metadata.is_some(),
                source: metadata.map(|m| m.visitor.source.as_str()),
                display_ip: metadata.map(|_| "[REDACTED]"),
            },
            capture_id: context.capture_id,
            generation,
            request_id: context.request_id,
            visitor_source: metadata.map(|m| m.visitor.source.as_str()),
            sequence: 0,
            method: String::new(),
            request_target: String::new(),
            status: None,
            state: State::Active,
            connection: GenerationState::Untracked,
            request: Message::new(self.inner.mode),
            response: Message::new(self.inner.mode),
            observation_incomplete: false,
        };
        let mut history = self.inner.history.lock();
        if self.inner.closed.load(Ordering::Acquire) {
            self.inner.active.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        let mut record = record;
        record.connection = generation_state(history.tunnel, generation);
        record.tunnel_id = (history.tunnel.generation == Some(generation))
            .then_some(history.tunnel_id)
            .flatten();
        history.insert(record, Instant::now());
        // Replay admission and row insertion share this lease, so an eviction
        // cannot forget a row before its raw consumer has even been admitted.
        let create = || {
            if raw_consumer {
                Capture::new(self.clone(), context)
            } else {
                Capture::display_only(self.clone(), context)
            }
        };
        if history.replay.is_some() {
            return Some(create());
        }
        drop(history);
        Some(create())
    }
    pub(super) fn update(&self, id: CaptureId, update: impl FnOnce(&mut InspectionRecord)) {
        if self.inner.closed.load(Ordering::Acquire) {
            return;
        }
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
            .map(|r| r.record.snapshot())
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
            .map(|r| r.record.snapshot())
            .collect()
    }
    pub fn activate(&self, generation: TunnelGeneration) {
        self.activate_identity(generation, None);
    }
    pub(crate) fn activate_tunnel(&self, generation: TunnelGeneration, tunnel_id: ulid::Ulid) {
        self.activate_identity(generation, Some(tunnel_id));
    }
    fn activate_identity(&self, generation: TunnelGeneration, tunnel_id: Option<ulid::Ulid>) {
        let mut h = self.inner.history.lock();
        if self.inner.closed.load(Ordering::Acquire) {
            return;
        }
        if let Some(replay) = &h.replay {
            replay.activate(generation);
        }
        h.tunnel_id = tunnel_id;
        h.tunnel = TunnelStatus {
            generation: Some(generation),
            connected: true,
        };
        let tunnel = h.tunnel;
        for row in &mut h.rows {
            row.record.connection = generation_state(tunnel, row.record.generation);
        }
    }
    pub fn disconnected(&self) {
        let mut h = self.inner.history.lock();
        if let Some(replay) = &h.replay {
            replay.deactivate();
        }
        h.tunnel.connected = false;
        let tunnel = h.tunnel;
        for row in &mut h.rows {
            row.record.connection = generation_state(tunnel, row.record.generation);
        }
    }
    pub fn sweep(&self) {
        self.inner.history.lock().sweep(Instant::now());
    }
    pub fn shutdown(&self) {
        self.inner.closed.store(true, Ordering::Release);
        let mut h = self.inner.history.lock();
        h.tunnel.connected = false;
        if let Some(replay) = &h.replay {
            replay.shutdown();
        }
        h.rows.clear();
    }
    /// Row reachability, TTL/eviction revocation and controller acceptance are
    /// one critical section. Never use a detached `get()` snapshot for authority.
    pub(super) fn with_replay_row<T>(
        &self,
        id: CaptureId,
        operation: impl FnOnce(
            &crate::replay::ReplayController,
            &InspectionRecord,
        ) -> Result<T, crate::replay::ReplayError>,
    ) -> Result<T, crate::replay::ReplayError> {
        use crate::replay::ReplayError;
        let mut h = self
            .inner
            .history
            .try_lock()
            .ok_or(ReplayError::Overloaded)?;
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(ReplayError::TunnelUnavailable);
        }
        h.sweep(Instant::now());
        let row = h
            .rows
            .iter()
            .find(|r| r.record.capture_id == id)
            .ok_or(ReplayError::NotRetained)?;
        let replay = h.replay.as_ref().ok_or(ReplayError::Disabled)?;
        let result = operation(replay, &row.record);
        drop(h);
        result
    }
    /// Consumed tickets recover safe metadata even after display eviction. Every
    /// new acceptance still requires the row and controller under this lease.
    pub(super) fn confirm_replay(
        &self,
        id: CaptureId,
        replay_id: ulid::Ulid,
        token: &str,
        confirm_side_effects: bool,
    ) -> Result<crate::replay::ReplayOutcome, crate::replay::ReplayError> {
        use crate::replay::ReplayError;
        let mut h = self
            .inner
            .history
            .try_lock()
            .ok_or(ReplayError::Overloaded)?;
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(ReplayError::TunnelUnavailable);
        }
        h.sweep(Instant::now());
        let controller = h.replay.as_ref().ok_or(ReplayError::Disabled)?;
        if let Some(outcome) =
            controller.consumed_confirmation(id, replay_id, token, confirm_side_effects)?
        {
            return Ok(outcome);
        }
        if !h.rows.iter().any(|row| row.record.capture_id == id) {
            return Err(ReplayError::NotRetained);
        }
        let result = controller.confirm(id, replay_id, token, confirm_side_effects);
        drop(h);
        result
    }
    pub(super) fn replay_outcome(
        &self,
        id: ulid::Ulid,
    ) -> Result<Option<crate::replay::ReplayOutcome>, crate::replay::ReplayError> {
        use crate::replay::ReplayError;
        let mut h = self
            .inner
            .history
            .try_lock()
            .ok_or(ReplayError::Overloaded)?;
        h.sweep(Instant::now());
        Ok(h.replay.as_ref().ok_or(ReplayError::Disabled)?.outcome(id))
    }
    pub(super) fn api_status(&self) -> Result<(usize, TunnelStatus), ()> {
        let mut h = self.inner.history.try_lock().ok_or(())?;
        h.sweep(Instant::now());
        Ok((h.rows.len(), h.tunnel))
    }
    pub(super) fn api_detail(&self, id: CaptureId) -> Result<Option<super::model::Detail>, ()> {
        let mut h = self.inner.history.try_lock().ok_or(())?;
        h.sweep(Instant::now());
        let Some(row) = h.rows.iter().find(|row| row.record.capture_id == id) else {
            return Ok(None);
        };
        let record = row.record.snapshot();
        let (replay, last_replay_outcome) = h.replay.as_ref().map_or_else(
            || {
                (
                    crate::replay::ReplayAvailability {
                        state: crate::replay::AvailabilityState::Disabled,
                        reason: Some(crate::replay::ReplayError::Disabled),
                        request_body_bytes: None,
                        raw_expires_at_unix_ms: None,
                    },
                    None,
                )
            },
            |replay| (replay.availability(id), replay.latest_outcome(id)),
        );
        drop(h);
        Ok(Some(super::model::Detail {
            record,
            replay: replay.into(),
            last_replay_outcome,
        }))
    }
    // At most one charged 64-KiB record clone at a time; search and serialization
    // run outside the history lock. Never call legacy search() from the service.
    pub(super) fn api_list(&self, filter: &super::server::Filter) -> Result<Vec<Summary>, ()> {
        let mut cursor = filter.before.unwrap_or(u64::MAX);
        let mut summaries = Vec::with_capacity(filter.limit);
        let mut budget = 60000usize;
        for _ in 0..MAX_RECORDS {
            let record = {
                let mut h = self.inner.history.try_lock().ok_or(())?;
                h.sweep(Instant::now());
                h.rows
                    .iter()
                    .rev()
                    .find(|row| row.record.sequence < cursor)
                    .map(|row| row.record.snapshot())
            };
            let Some(r) = record else {
                break;
            };
            cursor = r.sequence;
            if !filter.matches(&r) {
                continue;
            }
            let cost = 1024 + 6 * (r.method.len() + r.request_target.len());
            if cost > budget {
                break;
            }
            budget -= cost;
            summaries.push(Summary {
                started_at_ms: r.started_at_ms,
                duration_ms: r.duration_ms,
                response_source: r.response_source,
                request_boundary: r.request_boundary,
                failure_kind: r.failure_kind,
                bytes_to_backend: r.bytes_to_backend,
                bytes_from_backend: r.bytes_from_backend,
                capture_id: r.capture_id,
                generation: r.generation,
                request_id: r.request_id,
                sequence: r.sequence,
                method: r.method,
                request_target: r.request_target,
                status: r.status,
                state: r.state,
                connection: r.connection,
            });
            if summaries.len() == filter.limit {
                break;
            }
        }
        Ok(summaries)
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

fn generation_state(tunnel: TunnelStatus, generation: TunnelGeneration) -> GenerationState {
    match tunnel.generation {
        None => GenerationState::Untracked,
        Some(current) if current != generation => GenerationState::Superseded,
        Some(_) if tunnel.connected => GenerationState::Connected,
        Some(_) => GenerationState::Disconnected,
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    #[test]
    fn disconnect_reattach_and_recreate_do_not_falsify_old_task_completion() {
        let inspection = Inspection::new(PreviewMode::Off);
        let first = TunnelGeneration::new();
        let next = TunnelGeneration::new();
        inspection.activate(first);
        let mut old = inspection.admit(first, None).unwrap();
        old.request(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n");
        inspection.disconnected();
        let record = inspection.get(old.id()).unwrap();
        assert_eq!(record.connection, GenerationState::Disconnected);
        assert_eq!(record.state, State::Active);
        inspection.activate(first);
        assert_eq!(
            inspection.get(old.id()).unwrap().connection,
            GenerationState::Connected
        );
        inspection.activate(next);
        assert_eq!(
            inspection.get(old.id()).unwrap().connection,
            GenerationState::Superseded
        );
        old.response(b"HTTP/1.1 204 No Content\r\n\r\n");
        old.finish();
        let record = inspection.get(old.id()).unwrap();
        assert_eq!(record.state, State::Complete);
        assert_eq!(record.connection, GenerationState::Superseded);
        let late = inspection.admit(first, None).unwrap();
        assert_eq!(
            inspection.get(late.id()).unwrap().connection,
            GenerationState::Superseded
        );
        inspection.shutdown();
        drop(old);
        drop(late);
        assert!(inspection.search("").is_empty());
        assert!(inspection.admit(next, None).is_none());
        inspection.activate(next);
        assert!(!inspection.api_status().unwrap().1.connected);
    }
    #[tokio::test(start_paused = true)]
    async fn periodic_sweep_expires_idle_rows_without_a_read() {
        let inspection = Inspection::new(PreviewMode::Off);
        let service = super::super::InspectorService::start(inspection.clone(), 0)
            .await
            .unwrap();
        let capture = inspection.admit(TunnelGeneration::new(), None).unwrap();
        tokio::task::yield_now().await;
        inspection.inner.history.lock().rows[0].admitted = Instant::now().checked_sub(TTL).unwrap();
        tokio::time::advance(Duration::from_secs(2)).await;
        tokio::task::yield_now().await;
        assert!(inspection.inner.history.lock().rows.is_empty());
        drop(capture);
        service.shutdown().await;
    }
}

#[cfg(test)]
mod replay_lease_tests {
    use super::*;
    #[tokio::test]
    async fn replay_authority_holds_display_lease_and_eviction_forgets_raw() {
        let bound = super::super::BoundInspector::bind(0).await.unwrap();
        let replay = crate::replay::ReplayController::new(true, bound.addr().port()).unwrap();
        let history = Inspection::with_replay(PreviewMode::Off, replay.clone());
        let generation = TunnelGeneration::new();
        history.activate(generation);
        let mut capture = history
            .admit_backend(generation, None, "127.0.0.1:3000".parse().unwrap())
            .unwrap();
        let id = capture.id();
        capture.request(b"GET / HTTP/1.1\r\nHost: fixed.example\r\n\r\n");
        capture.response(b"HTTP/1.1 204 No Content\r\n\r\n");
        capture.finish();
        let preview = history
            .with_replay_row(id, |controller, row| {
                assert_eq!(row.capture_id, id);
                assert!(
                    history.inner.history.try_lock().is_none(),
                    "authority must hold the history lease"
                );
                controller.prepare(id)
            })
            .unwrap();
        let confirm =
            || history.confirm_replay(id, preview.replay_id, &preview.confirmation_token, true);
        history.inner.closed.store(true, Ordering::Release);
        assert_eq!(
            confirm().unwrap_err(),
            crate::replay::ReplayError::TunnelUnavailable
        );
        assert_eq!(
            history
                .with_replay_row(id, |controller, _| controller.prepare(id))
                .unwrap_err(),
            crate::replay::ReplayError::TunnelUnavailable
        );
        history.inner.closed.store(false, Ordering::Release);
        let mut lock = history.inner.history.lock();
        assert_eq!(
            confirm().unwrap_err(),
            crate::replay::ReplayError::Overloaded
        );
        assert_eq!(
            history
                .with_replay_row(id, |controller, _| controller.prepare(id))
                .unwrap_err(),
            crate::replay::ReplayError::Overloaded
        );
        lock.pop_front();
        drop(lock);
        assert_eq!(replay.raw_bytes(), 0);
        assert_eq!(
            confirm().unwrap_err(),
            crate::replay::ReplayError::NotRetained
        );
        assert_eq!(
            history
                .with_replay_row(id, |controller, _| controller.prepare(id))
                .unwrap_err(),
            crate::replay::ReplayError::NotRetained
        );
        history.shutdown();
    }
    #[tokio::test(start_paused = true)]
    async fn idle_service_eviction_forgets_raw_without_an_api_read() {
        let bound = super::super::BoundInspector::bind(0).await.unwrap();
        let replay = crate::replay::ReplayController::new(true, bound.addr().port()).unwrap();
        let history = Inspection::with_replay(PreviewMode::Off, replay.clone());
        let generation = TunnelGeneration::new();
        history.activate(generation);
        let mut capture = history
            .admit_backend(generation, None, "127.0.0.1:3000".parse().unwrap())
            .unwrap();
        capture.request(b"GET / HTTP/1.1\r\nHost: fixed.example\r\n\r\n");
        capture.response(b"HTTP/1.1 204 No Content\r\n\r\n");
        capture.finish();
        assert!(replay.raw_bytes() > 0);
        let service = bound.start(history.clone());
        history.inner.history.lock().rows[0].admitted = Instant::now().checked_sub(TTL).unwrap();
        tokio::time::advance(Duration::from_secs(2)).await;
        tokio::task::yield_now().await;
        assert!(history.inner.history.lock().rows.is_empty());
        assert_eq!(replay.raw_bytes(), 0);
        service.shutdown().await;
    }
}

#[cfg(test)]
impl Inspection {
    pub(super) fn allocated_capacity(&self) -> usize {
        let h = self.inner.history.lock();
        let records = h
            .rows
            .iter()
            .map(|row| {
                let capacity = row.record.allocated_capacity();
                assert!(capacity <= RECORD_RESERVATION);
                capacity
            })
            .sum::<usize>();
        // Deliberately double-count inline row/record storage, rather than
        // understating capacity by summing only serialized lengths.
        let bytes = records
            + h.rows.capacity() * std::mem::size_of::<Row>()
            + std::mem::size_of::<Inner>()
            + 2 * std::mem::size_of::<usize>();
        drop(h);
        assert!(bytes <= HISTORY_BYTES);
        bytes
    }
}
