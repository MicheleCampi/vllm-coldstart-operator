//! Bounds one reconcile, so that an API call the server never answers
//! fails the reconcile instead of holding it for the client's read
//! timeout (295 s in kube-client 2.0.1). See ADR-0016.

use std::future::Future;
use std::time::Duration;

/// How long one reconcile may run before it fails and is requeued.
pub const RECONCILE_TIMEOUT: Duration = Duration::from_secs(30);

/// Runs `fut`, or returns `None` if it has not finished within `limit`.
pub async fn bounded<F: Future>(fut: F, limit: Duration) -> Option<F::Output> {
    tokio::time::timeout(limit, fut).await.ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_call_never_answered_times_out_at_the_limit() {
        let start = tokio::time::Instant::now();
        let out = bounded(std::future::pending::<()>(), RECONCILE_TIMEOUT).await;
        assert!(out.is_none());
        let waited = start.elapsed();
        assert!(waited >= RECONCILE_TIMEOUT && waited < RECONCILE_TIMEOUT + Duration::from_secs(1));
    }

    #[tokio::test(start_paused = true)]
    async fn a_call_answered_in_time_returns_its_output() {
        assert_eq!(bounded(async { 7 }, RECONCILE_TIMEOUT).await, Some(7));
    }
}
