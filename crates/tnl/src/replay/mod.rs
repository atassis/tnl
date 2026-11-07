//! Explicitly opted-in memory-only retention and one-attempt replay.
//!
//! This module owns no listener or UI. The authenticated inspector must check
//! sanitized-row existence before prepare/new execution, recover consumed-ticket
//! metadata before that existence gate, call `forget` on eviction,
//! `activate` on each trusted attach, `deactivate` on disconnect and `shutdown`
//! on service failure. Its periodic timer must call `sweep` even with no traffic.
//! Raw eligibility is independently enforced here; display bytes are never input.
mod engine;
mod model;
mod retention;
use crate::inspection::{
    CaptureContext, CaptureId, ConsumerFactory, Direction, Observation, ObservationConsumer,
    TunnelGeneration,
};
pub use model::*;
use parking_lot::Mutex;
use retention::{Charge, RawRequest};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Weak,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio_util::sync::CancellationToken;

const TICKET_TTL: Duration = Duration::from_secs(30);
const OUTCOME_TTL: Duration = Duration::from_secs(15 * 60);
struct Entry {
    context: CaptureContext,
    epoch: u64,
    revision: ulid::Ulid,
    admitted: Instant,
    expires_wall: u64,
    raw: Arc<RawRequest>,
}
struct Ticket {
    capture: CaptureId,
    generation: TunnelGeneration,
    epoch: u64,
    replay: ulid::Ulid,
    revision: ulid::Ulid,
    token: [u8; 32],
    created: Instant,
}
struct SavedOutcome {
    token: [u8; 32],
    created: Instant,
    outcome: ReplayOutcome,
}
struct State {
    enabled: bool,
    generation: Option<TunnelGeneration>,
    active: bool,
    epoch: u64,
    entries: VecDeque<Entry>,
    tickets: Vec<Ticket>,
    outcomes: VecDeque<SavedOutcome>,
    execution: Option<(ulid::Ulid, CancellationToken)>,
}
struct Inner {
    state: Mutex<State>,
    budget: Arc<AtomicUsize>,
    slots: Arc<AtomicUsize>,
    inspector_port: u16,
}
/// Safe metadata facade. No raw getter, raw Debug/Serialize or display fallback.
#[derive(Clone)]
pub struct ReplayController {
    inner: Arc<Inner>,
}
impl std::fmt::Debug for ReplayController {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplayController")
            .field("charged_bytes", &self.raw_bytes())
            .finish_non_exhaustive()
    }
}
impl ReplayController {
    /// Consent is independent of inspection/body-preview configuration. The
    /// already-bound inspector port is immutable and must be nonzero.
    pub fn new(retain_secrets: bool, inspector_port: u16) -> Result<Self, ReplayError> {
        if inspector_port == 0 {
            return Err(ReplayError::LocalTargetRequired);
        }
        Ok(Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    enabled: retain_secrets,
                    generation: None,
                    active: false,
                    epoch: 0,
                    entries: VecDeque::with_capacity(20),
                    tickets: Vec::with_capacity(8),
                    outcomes: VecDeque::with_capacity(32),
                    execution: None,
                }),
                budget: Arc::new(AtomicUsize::new(0)),
                slots: Arc::new(AtomicUsize::new(0)),
                inspector_port,
            }),
        })
    }
    /// Trusted lifecycle authority, not a browser-controlled generation setter.
    /// Reattachment retains completed captures, but rotates the active epoch and
    /// invalidates every ticket and unfinished old consumer.
    pub fn activate(&self, generation: TunnelGeneration) {
        let mut state = self.inner.state.lock();
        if state.generation == Some(generation) {
            state.entries.retain(|e| e.raw.complete());
        } else {
            state.entries.clear();
        }
        state.epoch = state.epoch.wrapping_add(1);
        state.generation = Some(generation);
        state.active = true;
        state.tickets.clear();
        Self::sweep_locked(&mut state, Instant::now());
        drop(state);
    }
    pub fn deactivate(&self) {
        let mut state = self.inner.state.lock();
        state.active = false;
        state.epoch = state.epoch.wrapping_add(1);
        state.tickets.clear();
        state.entries.retain(|e| e.raw.complete());
    }
    /// Service/process shutdown is permanent for this controller, including
    /// admission. Cancellation does not assert that no request bytes were sent.
    pub fn shutdown(&self) {
        let mut state = self.inner.state.lock();
        state.enabled = false;
        state.active = false;
        state.epoch = state.epoch.wrapping_add(1);
        state.entries.clear();
        state.tickets.clear();
        if let Some((_, cancel)) = &state.execution {
            cancel.cancel();
        }
    }
    pub fn forget(&self, id: CaptureId) {
        let mut state = self.inner.state.lock();
        state.entries.retain(|e| e.context.capture_id != id);
        state.tickets.retain(|t| t.capture != id);
    }
    pub fn sweep(&self) {
        self.sweep_at(Instant::now());
    }
    fn sweep_at(&self, now: Instant) {
        Self::sweep_locked(&mut self.inner.state.lock(), now);
    }
    fn sweep_locked(state: &mut State, now: Instant) {
        state
            .entries
            .retain(|e| now.saturating_duration_since(e.admitted) < retention::TTL);
        state
            .tickets
            .retain(|t| now.saturating_duration_since(t.created) < TICKET_TTL);
        state.outcomes.retain(|o| {
            o.outcome.state == AttemptState::Running
                || now.saturating_duration_since(o.created) < OUTCOME_TTL
        });
    }
    pub fn raw_bytes(&self) -> usize {
        self.inner.budget.load(Ordering::Acquire)
    }
    fn eligible<'a>(&self, state: &'a State, id: CaptureId) -> Result<&'a Entry, ReplayError> {
        if !state.enabled {
            return Err(ReplayError::Disabled);
        }
        if !state.active {
            return Err(ReplayError::TunnelUnavailable);
        }
        let entry = state
            .entries
            .iter()
            .find(|e| e.context.capture_id == id)
            .ok_or(ReplayError::NotRetained)?;
        if state.generation != Some(entry.context.generation) {
            return Err(ReplayError::WrongGeneration);
        }
        if !entry
            .context
            .backend
            .is_some_and(|b| retention::safe_backend(b, self.inner.inspector_port))
        {
            return Err(ReplayError::LocalTargetRequired);
        }
        if !entry.raw.complete() {
            return Err(ReplayError::Incomplete);
        }
        Ok(entry)
    }
    pub fn availability(&self, id: CaptureId) -> ReplayAvailability {
        let mut state = self.inner.state.lock();
        Self::sweep_locked(&mut state, Instant::now());
        match self.eligible(&state, id) {
            Ok(e) => ReplayAvailability {
                state: AvailabilityState::Eligible,
                reason: None,
                request_body_bytes: Some(e.raw.body_len()),
                raw_expires_at_unix_ms: Some(e.expires_wall),
            },
            Err(reason) => ReplayAvailability {
                state: match reason {
                    ReplayError::Disabled => AvailabilityState::Disabled,
                    ReplayError::Incomplete => AvailabilityState::Pending,
                    _ => AvailabilityState::Unavailable,
                },
                reason: Some(reason),
                request_body_bytes: None,
                raw_expires_at_unix_ms: None,
            },
        }
    }
    /// Produces no backend I/O and pins no raw payload. ALL methods require the
    /// later explicit side-effects acknowledgement and this expiring ticket.
    pub fn prepare(&self, id: CaptureId) -> Result<ReplayPreview, ReplayError> {
        let mut state = self.inner.state.lock();
        Self::sweep_locked(&mut state, Instant::now());
        let e = self.eligible(&state, id)?;
        let generation = e.context.generation;
        let target = e.context.backend.ok_or(ReplayError::LocalTargetRequired)?;
        let expires = e.expires_wall;
        let len = e.raw.body_len();
        let revision = e.revision;
        if state.tickets.len() == 8 {
            return Err(ReplayError::Overloaded);
        }
        let mut token = [0; 32];
        getrandom::fill(&mut token).map_err(|_| ReplayError::Overloaded)?;
        let replay = ulid::Ulid::new();
        let epoch = state.epoch;
        state.tickets.push(Ticket {
            capture: id,
            generation,
            epoch,
            replay,
            revision,
            token,
            created: Instant::now(),
        });
        drop(state);
        Ok(ReplayPreview {
            capture_id: id,
            tunnel_generation: generation,
            replay_id: replay,
            confirmation_token: encode_token(&token),
            confirmation_expires_at_unix_ms: wall_ms().saturating_add(30_000),
            raw_expires_at_unix_ms: expires,
            request_body_bytes: len,
            target,
            host_policy: "captured_effective",
            warnings: [
                "potential_side_effects",
                "original_secrets_retained",
                "signature_may_expire",
            ],
        })
    }
    /// Metadata-only duplicate recovery. This never accesses raw entries,
    /// admits retention, or starts an execution; new tickets return None.
    pub(crate) fn consumed_confirmation(
        &self,
        capture: CaptureId,
        replay: ulid::Ulid,
        token: &str,
        confirm_side_effects: bool,
    ) -> Result<Option<ReplayOutcome>, ReplayError> {
        if !confirm_side_effects {
            return Err(ReplayError::ConfirmationRequired);
        }
        let token = decode_token(token).ok_or(ReplayError::InvalidConfirmation)?;
        Ok(consumed_outcome(
            &self.inner.state.lock(),
            capture,
            replay,
            &token,
            Instant::now(),
        ))
    }
    /// Atomic lease acceptance: authority, ticket, snapshot pin, raw/transient
    /// quota and the one-execution permit share this single state lock. Budget
    /// drops use atomics only (never acquire the state lock); cancellation only
    /// signals under this lock, and task completion acquires it after I/O ends.
    /// A transition after acceptance cannot retarget or restart the operation.
    pub fn confirm(
        &self,
        capture: CaptureId,
        replay: ulid::Ulid,
        token: &str,
        confirm_side_effects: bool,
    ) -> Result<ReplayOutcome, ReplayError> {
        if !confirm_side_effects {
            return Err(ReplayError::ConfirmationRequired);
        }
        let token = decode_token(token).ok_or(ReplayError::InvalidConfirmation)?;
        let mut state = self.inner.state.lock();
        let now = Instant::now();
        Self::sweep_locked(&mut state, now);
        if let Some(outcome) = consumed_outcome(&state, capture, replay, &token, now) {
            return Ok(outcome);
        }
        let index = state
            .tickets
            .iter()
            .position(|t| t.token == token && t.replay == replay && t.capture == capture)
            .ok_or(ReplayError::InvalidConfirmation)?;
        let ticket = &state.tickets[index];
        if ticket.epoch != state.epoch || state.generation != Some(ticket.generation) {
            return Err(ReplayError::InvalidConfirmation);
        }
        let entry = self.eligible(&state, capture)?;
        if entry.revision != ticket.revision {
            return Err(ReplayError::InvalidConfirmation);
        }
        let raw = entry.raw.clone();
        let context = entry.context;
        if state.execution.is_some() {
            return Err(ReplayError::Busy);
        }
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|_| ReplayError::RuntimeUnavailable)?;
        // The execution pin remains charged even if pressure/expiry removes the
        // discoverable entry. Eviction can reclaim only genuinely unpinned data.
        let charge = reserve_with_eviction(&self.inner, &mut state, retention::ENGINE_CHARGE)
            .ok_or(ReplayError::Overloaded)?;
        let cancel = CancellationToken::new();
        let outcome = ReplayOutcome {
            replay_id: replay,
            parent_capture_id: capture,
            tunnel_generation: context.generation,
            state: AttemptState::Running,
            delivery: Delivery::Unknown,
            status: None,
            duration_ms: 0,
            request_body_bytes: raw.body_len(),
            response_body_bytes: 0,
            response_complete: false,
            error_code: None,
        };
        state.tickets.swap_remove(index);
        if state.outcomes.len() == 32 {
            state.outcomes.pop_front();
        }
        state.outcomes.push_back(SavedOutcome {
            token,
            created: Instant::now(),
            outcome: outcome.clone(),
        });
        state.execution = Some((replay, cancel.clone()));
        drop(state);
        let inner = Arc::downgrade(&self.inner);
        let initial = outcome.clone();
        let port = self.inner.inspector_port;
        runtime.spawn(async move {
            let result =
                Box::pin(engine::execute(raw, context.backend, port, cancel, initial)).await;
            drop(charge);
            if let Some(inner) = inner.upgrade() {
                let mut state = inner.state.lock();
                if let Some(saved) = state
                    .outcomes
                    .iter_mut()
                    .find(|o| o.outcome.replay_id == replay)
                {
                    saved.outcome = result;
                    saved.created = Instant::now();
                }
                if state
                    .execution
                    .as_ref()
                    .is_some_and(|(id, _)| *id == replay)
                {
                    state.execution = None;
                }
            }
        });
        Ok(outcome)
    }
    /// Bounded safe metadata only, for the sanitized parent detail view.
    pub(crate) fn latest_outcome(&self, capture: CaptureId) -> Option<ReplayOutcome> {
        let mut state = self.inner.state.lock();
        Self::sweep_locked(&mut state, Instant::now());
        state
            .outcomes
            .iter()
            .rev()
            .find(|saved| saved.outcome.parent_capture_id == capture)
            .map(|saved| saved.outcome.clone())
    }
    pub fn outcome(&self, replay: ulid::Ulid) -> Option<ReplayOutcome> {
        let mut state = self.inner.state.lock();
        Self::sweep_locked(&mut state, Instant::now());
        state
            .outcomes
            .iter()
            .find(|o| o.outcome.replay_id == replay)
            .map(|o| o.outcome.clone())
    }
}
fn consumed_outcome(
    state: &State,
    capture: CaptureId,
    replay: ulid::Ulid,
    token: &[u8; 32],
    now: Instant,
) -> Option<ReplayOutcome> {
    state
        .outcomes
        .iter()
        .find(|saved| {
            &saved.token == token
                && saved.outcome.replay_id == replay
                && saved.outcome.parent_capture_id == capture
                && (saved.outcome.state == AttemptState::Running
                    || now.saturating_duration_since(saved.created) < OUTCOME_TTL)
        })
        .map(|saved| saved.outcome.clone())
}
fn reserve_with_eviction(inner: &Inner, state: &mut State, bytes: usize) -> Option<Charge> {
    loop {
        if let Some(charge) = Charge::reserve(&inner.budget, bytes) {
            return Some(charge);
        }
        state.entries.pop_front()?;
    }
}
impl ConsumerFactory for ReplayController {
    fn admit(&self, context: CaptureContext) -> Option<Box<dyn ObservationConsumer>> {
        let mut state = self.inner.state.lock();
        Self::sweep_locked(&mut state, Instant::now());
        if !state.enabled
            || !state.active
            || state.generation != Some(context.generation)
            || !context
                .backend
                .is_some_and(|b| retention::safe_backend(b, self.inner.inspector_port))
            || state
                .entries
                .iter()
                .any(|e| e.context.capture_id == context.capture_id)
        {
            return None;
        }
        while state.entries.len() >= retention::MAX_ENTRIES {
            state.entries.pop_front();
        }
        while self.inner.slots.load(Ordering::Acquire) >= retention::MAX_ENTRIES {
            state.entries.pop_front()?;
        }
        let charge = reserve_with_eviction(&self.inner, &mut state, retention::HEAD_CHARGE)?;
        self.inner.slots.fetch_add(1, Ordering::AcqRel);
        let revision = ulid::Ulid::new();
        let epoch = state.epoch;
        state.entries.push_back(Entry {
            context,
            epoch,
            revision,
            admitted: Instant::now(),
            expires_wall: wall_ms().saturating_add(600_000),
            raw: Arc::new(RawRequest::new(charge, self.inner.slots.clone())),
        });
        drop(state);
        Some(Box::new(RawConsumer {
            inner: Arc::downgrade(&self.inner),
            id: context.capture_id,
            epoch,
            revision,
        }))
    }
}
struct RawConsumer {
    inner: Weak<Inner>,
    id: CaptureId,
    epoch: u64,
    revision: ulid::Ulid,
}
impl ObservationConsumer for RawConsumer {
    fn observe(&mut self, event: &Observation<'_>) {
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let mut state = inner.state.lock();
        ReplayController::sweep_locked(&mut state, Instant::now());
        let Some(index) = state.entries.iter().position(|e| {
            e.context.capture_id == self.id && e.epoch == self.epoch && e.revision == self.revision
        }) else {
            return;
        };
        // Revocation always wins, including repeated stops after provisional End.
        if matches!(
            event,
            Observation::Stopped(Direction::Request, _)
                | Observation::Stopped(Direction::Response, "cancelled")
                | Observation::ResponseHead { status: 101, .. }
        ) {
            state.entries.remove(index);
            state.tickets.retain(|t| t.capture != self.id);
            return;
        }
        if state.epoch != self.epoch || !state.active {
            return;
        }
        let Some(mut entry) = state.entries.remove(index) else {
            return;
        };
        let Some(raw) = Arc::get_mut(&mut entry.raw) else {
            state.entries.insert(index, entry);
            return;
        };
        let valid = match event {
            Observation::RequestHead {
                method,
                target,
                headers,
            } => raw.head(method, target, headers).is_ok(),
            Observation::Body(Direction::Request, bytes) => {
                if !raw.head_seen
                    || raw.ended
                    || raw.finished
                    || raw.body_len().saturating_add(bytes.len()) > retention::BODY_LIMIT
                {
                    false
                } else if bytes.is_empty() {
                    true
                } else {
                    let admitted = if raw.building.capacity() == 0 {
                        if let Some(charge) =
                            reserve_with_eviction(&inner, &mut state, retention::BODY_LIMIT)
                        {
                            raw.charges.push(charge);
                            raw.building = Vec::with_capacity(retention::BODY_LIMIT);
                            true
                        } else {
                            false
                        }
                    } else {
                        true
                    };
                    if admitted {
                        raw.building.extend_from_slice(bytes);
                    }
                    admitted
                }
            }
            Observation::MessageEnd {
                direction: Direction::Request,
                trailers,
            } => {
                let valid = raw.head_seen
                    && !raw.ended
                    && !*trailers
                    && raw.expected_length.is_none_or(|n| n == raw.body_len());
                if valid {
                    raw.ended = true;
                }
                valid
            }
            Observation::Finished => {
                if raw.finished {
                    true
                } else if raw.ended {
                    raw.finish();
                    true
                } else {
                    false
                }
            }
            _ => true,
        };
        if valid {
            let index = state
                .entries
                .iter()
                .position(|e| e.admitted > entry.admitted)
                .unwrap_or(state.entries.len());
            state.entries.insert(index, entry);
        } else {
            state.tickets.retain(|t| t.capture != self.id);
        }
    }
}
impl Drop for RawConsumer {
    fn drop(&mut self) {
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let mut state = inner.state.lock();
        state.entries.retain(|e| {
            e.context.capture_id != self.id
                || e.epoch != self.epoch
                || e.revision != self.revision
                || e.raw.complete()
        });
    }
}
fn wall_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}
fn encode_token(token: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut text = String::with_capacity(64);
    for byte in token {
        let _ = write!(text, "{byte:02x}");
    }
    text
}
fn decode_token(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let mut token = [0; 32];
    for (i, byte) in token.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(token)
}
#[cfg(test)]
mod tests;

#[cfg(test)]
impl ReplayController {
    pub(crate) fn allocated_capacity_without_execution(&self) -> usize {
        let state = self.inner.state.lock();
        assert!(state.execution.is_none());
        let mut bytes = std::mem::size_of::<Inner>()
            + 6 * std::mem::size_of::<usize>()
            + state.entries.capacity() * std::mem::size_of::<Entry>()
            + state.tickets.capacity() * std::mem::size_of::<Ticket>()
            + state.outcomes.capacity() * std::mem::size_of::<SavedOutcome>();
        for entry in &state.entries {
            // Bytes::from(Vec) retains its backing allocation. Charge the full
            // 1-MiB body reservation, NOT body.len(), for a finished entity.
            let body = if entry.raw.body.is_empty() {
                0
            } else {
                retention::BODY_LIMIT
            };
            bytes += std::mem::size_of::<RawRequest>()
                + 2 * std::mem::size_of::<usize>()
                + retention::HEAD_LIMIT
                + entry.raw.building.capacity()
                + body
                + entry.raw.charges.capacity() * std::mem::size_of::<Charge>();
        }
        drop(state);
        assert!(bytes <= self.raw_bytes() + 8192);
        bytes
    }
}
