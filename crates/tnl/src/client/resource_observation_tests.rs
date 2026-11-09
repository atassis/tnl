//! Private observations of real run ownership. `TaskTracker` clones share only
//! atomic bookkeeping/Notify (tokio-util 0.7.18); no tracked token or task owner
//! is created, and this observer never closes/waits the tracker.
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
#[derive(Default)]
pub(super) struct Resources {
    tracker: Mutex<Option<tokio_util::task::TaskTracker>>,
    handles: Arc<AtomicUsize>,
    accepts: Arc<AtomicUsize>,
    completed: Arc<AtomicUsize>,
}
tokio::task_local! { pub(super) static OBSERVE: Arc<Resources>; }
pub(super) fn run(tracker: &tokio_util::task::TaskTracker) {
    let _ = OBSERVE.try_with(|observer| *observer.tracker.lock().unwrap() = Some(tracker.clone()));
}
pub(super) struct Guard(Arc<AtomicUsize>, Option<Arc<AtomicUsize>>);
impl Guard {
    fn new(count: Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(count, None)
    }
    pub(super) fn physical_completed(&self) {
        if let Some(count) = &self.1 {
            count.fetch_add(1, Ordering::SeqCst);
        }
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
pub(super) fn generation() -> (Option<Guard>, Option<Guard>) {
    OBSERVE
        .try_with(|o| {
            o.handles.fetch_add(1, Ordering::SeqCst);
            (
                Some(Guard(o.handles.clone(), Some(o.completed.clone()))),
                Some(Guard::new(o.accepts.clone())),
            )
        })
        .unwrap_or((None, None))
}
impl Resources {
    pub(super) fn completed(&self) -> usize {
        self.completed.load(Ordering::SeqCst)
    }
    pub(super) fn snapshot(&self) -> (usize, usize, usize) {
        (
            self.tracker
                .lock()
                .unwrap()
                .as_ref()
                .map_or(0, tokio_util::task::TaskTracker::len),
            self.handles.load(Ordering::SeqCst),
            self.accepts.load(Ordering::SeqCst),
        )
    }
}
