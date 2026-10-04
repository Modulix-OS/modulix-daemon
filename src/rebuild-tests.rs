/// Tests for [`super`].
///
/// # Structure
/// - `unit` – mutual exclusion of the shared guard.
use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

mod unit {
    use super::*;

    /// Two holders never overlap: the second waits for the first to drop its
    /// guard, which is the whole point of the lock.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn guard_serialises_holders() {
        let inside = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let mut tasks = Vec::new();
        for _ in 0..8 {
            let inside = Arc::clone(&inside);
            let peak = Arc::clone(&peak);
            tasks.push(tokio::spawn(async move {
                let _guard = guard().await;
                let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::task::yield_now().await;
                inside.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for task in tasks {
            task.await.expect("task");
        }

        assert_eq!(peak.load(Ordering::SeqCst), 1);
    }

    /// The guard is released when dropped, so a later caller gets it. A lock
    /// left held would hang this test rather than fail it, which is the signal.
    #[tokio::test]
    async fn guard_is_released_on_drop() {
        {
            let _guard = guard().await;
        }
        let _guard = guard().await;
    }
}
