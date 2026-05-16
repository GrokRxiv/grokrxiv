//! Exponential backoff with jitter.
//!
//! Used by every provider for `429 Too Many Requests` and 5xx responses.

use std::time::Duration;

use rand::Rng;

use crate::LLMError;

/// Maximum attempts (initial + 3 retries).
pub const MAX_ATTEMPTS: u32 = 4;
/// Cap on per-retry sleep.
pub const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Run `f` up to [`MAX_ATTEMPTS`] times, sleeping `min(2^(attempt) * 250ms, 30s)`
/// plus jitter between attempts whenever the error is retryable.
///
/// If the provider supplied a `Retry-After` value via
/// [`LLMError::RateLimited`], that value is used instead of the exponential
/// delay (capped to [`MAX_BACKOFF`]).
pub async fn with_backoff<F, Fut, T>(mut f: F) -> Result<T, LLMError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, LLMError>>,
{
    let mut attempt: u32 = 0;
    loop {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) if !e.is_retryable() => return Err(e),
            Err(e) => {
                attempt += 1;
                if attempt >= MAX_ATTEMPTS {
                    return Err(e);
                }
                let sleep = match &e {
                    LLMError::RateLimited(Some(d)) => std::cmp::min(*d, MAX_BACKOFF),
                    _ => exp_backoff(attempt),
                };
                tracing::warn!(
                    attempt,
                    sleep_ms = sleep.as_millis() as u64,
                    err = %e,
                    "llm call failed; retrying",
                );
                tokio::time::sleep(sleep).await;
            }
        }
    }
}

fn exp_backoff(attempt: u32) -> Duration {
    let base_ms = 250u64.saturating_mul(1u64 << attempt.min(7));
    let jitter_ms = rand::thread_rng().gen_range(0..=base_ms / 2);
    let total = Duration::from_millis(base_ms.saturating_add(jitter_ms));
    std::cmp::min(total, MAX_BACKOFF)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn returns_immediately_on_success() {
        let n = with_backoff(|| async { Ok::<_, LLMError>(1u32) })
            .await
            .unwrap();
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn retries_then_succeeds() {
        let counter = Arc::new(AtomicU32::new(0));
        let c = counter.clone();
        let v: u32 = with_backoff(move || {
            let c = c.clone();
            async move {
                let n = c.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    Err(LLMError::RateLimited(Some(Duration::from_millis(1))))
                } else {
                    Ok(42)
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(v, 42);
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn gives_up_after_max_attempts() {
        let counter = Arc::new(AtomicU32::new(0));
        let c = counter.clone();
        let err = with_backoff(move || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err::<u32, _>(LLMError::RateLimited(Some(Duration::from_millis(1))))
            }
        })
        .await
        .unwrap_err();
        assert!(matches!(err, LLMError::RateLimited(_)));
        assert_eq!(counter.load(Ordering::SeqCst), MAX_ATTEMPTS);
    }

    #[tokio::test]
    async fn non_retryable_short_circuits() {
        let counter = Arc::new(AtomicU32::new(0));
        let c = counter.clone();
        let err = with_backoff(move || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err::<u32, _>(LLMError::Schema("nope".into()))
            }
        })
        .await
        .unwrap_err();
        assert!(matches!(err, LLMError::Schema(_)));
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
}
