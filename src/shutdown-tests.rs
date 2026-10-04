/// Tests for [`super`].
///
/// # Structure
/// - `unit` – admission control and the drain, with no real signal involved.
///
/// # Isolation
/// The module's state is process-wide and [`super::begin_shutdown`] is
/// one-way, so every test takes [`STATE_GUARD`] and calls
/// [`super::reset_for_tests`] first. Without that, the first test to begin a
/// shutdown would decide the result of all the others. The guard is a
/// `tokio` mutex because the async tests hold it across `await` points.
use super::*;
use tokio::sync::{Mutex as AsyncMutex, MutexGuard as AsyncMutexGuard};

/// Serialises the tests against the module's shared state.
static STATE_GUARD: AsyncMutex<()> = AsyncMutex::const_new(());

/// Takes [`STATE_GUARD`] and clears the module's state, from a synchronous
/// test.
///
/// # Pre-conditions
/// Must not be called from inside a runtime: it blocks the current thread.
///
/// # Returns
/// The guard, which must stay alive for the whole test.
fn fresh_state_blocking() -> AsyncMutexGuard<'static, ()> {
    let guard = STATE_GUARD.blocking_lock();
    reset_for_tests();
    guard
}

/// Takes [`STATE_GUARD`] and clears the module's state, from an async test.
///
/// # Returns
/// The guard, which must stay alive for the whole test.
async fn fresh_state() -> AsyncMutexGuard<'static, ()> {
    let guard = STATE_GUARD.lock().await;
    reset_for_tests();
    guard
}

mod unit {
    use super::*;

    /// A running daemon admits transactions.
    #[test]
    fn enter_succeeds_while_running() {
        let _guard = fresh_state_blocking();
        assert!(!is_shutting_down());
        assert!(enter().is_ok());
    }

    /// Once terminating, a new transaction is refused rather than started and
    /// killed halfway through.
    #[test]
    fn enter_refused_after_begin_shutdown() {
        let _guard = fresh_state_blocking();
        begin_shutdown();

        assert!(is_shutting_down());
        assert!(matches!(enter(), Err(Error::ShuttingDown)));
    }

    /// A refusal counts nothing, so it cannot hold the drain back.
    #[tokio::test]
    async fn refused_enter_does_not_block_the_drain() {
        let _guard = fresh_state().await;
        begin_shutdown();
        let _ = enter();

        wait_drained().await;
    }

    /// Nothing in flight drains immediately.
    #[tokio::test]
    async fn wait_drained_returns_when_idle() {
        let _guard = fresh_state().await;
        wait_drained().await;
    }

    /// The drain waits for the token to be dropped, and only then returns.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wait_drained_waits_for_the_token() {
        let _guard = fresh_state().await;
        let token = enter().expect("admitted");
        begin_shutdown();

        let drain = tokio::spawn(wait_drained());
        tokio::task::yield_now().await;
        assert!(!drain.is_finished());

        drop(token);
        drain.await.expect("drain");
    }

    /// Several transactions in flight: the drain returns when the last one is
    /// done, not when the first is.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wait_drained_waits_for_every_token() {
        let _guard = fresh_state().await;
        let first = enter().expect("admitted");
        let second = enter().expect("admitted");
        begin_shutdown();

        let drain = tokio::spawn(wait_drained());
        drop(first);
        tokio::task::yield_now().await;
        assert!(!drain.is_finished());

        drop(second);
        drain.await.expect("drain");
    }
}
