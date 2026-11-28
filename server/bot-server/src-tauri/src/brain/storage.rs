//! Shared finite blocking domain. A timed-out caller does not release a slot
//! while its actual SQLite closure is still running.
use std::{sync::{Arc,OnceLock},time::Duration};
use tokio::sync::Semaphore;
pub(crate) async fn run<T:Send+'static>(work:impl FnOnce()->T+Send+'static)->Result<T,String>{
    static SLOTS:OnceLock<Arc<Semaphore>>=OnceLock::new();
    let permit=SLOTS.get_or_init(||Arc::new(Semaphore::new(8))).clone().try_acquire_owned().map_err(|_|"storage_busy")?;
    let task=tokio::task::spawn_blocking(move||{let _permit=permit;work()});
    tokio::time::timeout(Duration::from_secs(3),task).await.map_err(|_|"storage_deadline".to_string())?
        .map_err(|_|"storage_worker_failed".to_string())
}
