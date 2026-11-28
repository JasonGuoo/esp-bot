//! The connection owns preparations; a Stop can retire their output without
//! waiting for cleanup. Dropping the connection aborts all owned async work.
use std::{future::Future, sync::{Arc, OnceLock}, time::Duration};
use tokio::{sync::Semaphore, task::JoinSet};

pub(super) struct Preparations { jobs: JoinSet<()>, slots: Arc<Semaphore> }
impl Preparations {
    pub fn new() -> Self { Self { jobs: JoinSet::new(), slots: Arc::new(Semaphore::new(4)) } }
    pub fn spawn(&mut self, work: impl Future<Output=()> + Send + 'static) -> Result<(), String> {
        // Finished handles are reaped before admission, not accumulated forever.
        while self.jobs.try_join_next().is_some() {}
        let permit = self.slots.clone().try_acquire_owned().map_err(|_| "presentation_prepare_busy")?;
        self.jobs.spawn(async move { let _permit = permit; work.await });
        Ok(())
    }
    pub fn is_empty(&self) -> bool { self.jobs.is_empty() }
    pub async fn reap(&mut self) { let _ = self.jobs.join_next().await; }
}

pub(super) use super::super::storage::run as storage;

pub(super) async fn prepare<T>(work: impl Future<Output=Result<T, String>>) -> Result<T, String> {
    tokio::time::timeout(Duration::from_secs(30), work).await
        .map_err(|_| "presentation_prepare_timeout".to_string())?
}

/// Cleanup has a separate quota from reads and speech. The local fence must
/// already be advanced; callers pass exact retired lease IDs, never a predicate
/// that could match an output granted after this command was queued.
pub(super) fn cleanup(work: impl FnOnce() + Send + 'static) -> Result<(), String> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    let permit = SLOTS.get_or_init(|| Arc::new(Semaphore::new(8))).clone()
        .try_acquire_owned().map_err(|_| "cleanup_busy")?;
    tokio::task::spawn_blocking(move || { let _permit = permit; work() });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn stalled_preparations_are_bounded_and_owned_by_the_connection() {
        struct Exit(Arc<std::sync::atomic::AtomicUsize>);
        impl Drop for Exit { fn drop(&mut self) { self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst); } }
        let exited = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut jobs = Preparations::new();
        for _ in 0..4 {
            let guard = Exit(exited.clone());
            jobs.spawn(async move { let _guard = guard; std::future::pending::<()>().await }).unwrap();
        }
        assert_eq!(jobs.spawn(async {}), Err("presentation_prepare_busy".into()));
        // A ready control future runs despite every preparation being pending.
        assert!(tokio::time::timeout(Duration::from_millis(50), async { true }).await.unwrap());
        drop(jobs);
        for _ in 0..10 { tokio::task::yield_now().await; }
        assert_eq!(exited.load(std::sync::atomic::Ordering::SeqCst), 4);
    }
}
