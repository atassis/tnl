//! Pause only generation two after its first actual acceptance, allowing the
//! real driver's bounded inbound delivery to overflow without scheduler races.
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct AcceptGate {
    generations: AtomicUsize,
    accepts: AtomicUsize,
    pub(crate) entered: tokio_util::sync::CancellationToken,
    pub(crate) release: tokio_util::sync::CancellationToken,
    pub(crate) termination: Mutex<Option<tnl_protocol::SessionTermination>>,
}

tokio::task_local! {
    pub(crate) static TEST_ACCEPT_GATE: Arc<AcceptGate>;
    static ACTIVE_ACCEPT_GATE: Option<Arc<AcceptGate>>;
}

pub(super) fn scope<F: Future>(
    future: F,
    termination: tnl_protocol::SessionTermination,
) -> impl Future<Output = F::Output> {
    // Capture the parent task's gate BEFORE spawning the accept owner.
    let gate = TEST_ACCEPT_GATE
        .try_with(|gate| {
            (gate.generations.fetch_add(1, Ordering::SeqCst) == 1).then(|| {
                *gate.termination.lock().unwrap() = Some(termination);
                gate.clone()
            })
        })
        .ok()
        .flatten();
    async move { ACTIVE_ACCEPT_GATE.scope(gate, future).await }
}

pub(super) async fn before_accept() {
    let gate = ACTIVE_ACCEPT_GATE.try_with(Clone::clone).ok().flatten();
    if let Some(gate) = gate {
        if gate.accepts.fetch_add(1, Ordering::SeqCst) != 0 {
            gate.entered.cancel();
            gate.release.cancelled().await;
        }
    }
}
