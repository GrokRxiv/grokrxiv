//! Single-flight gate enforcing arXiv API spacing.
//!
//! Even though `grokrxiv-ingest` enforces its own internal spacing, the
//! orchestrator wraps every ingest call with this gate so we are courteous
//! across crate boundaries.

use std::time::{Duration, Instant};

use tokio::sync::{Mutex, Semaphore};

/// Shared gate that allows one in-flight arXiv call at a time and enforces a
/// minimum interval between consecutive calls.
pub struct ArxivGate {
    sem: Semaphore,
    last: Mutex<Option<Instant>>,
    min_gap: Duration,
}

impl ArxivGate {
    /// New gate with a `min_gap` between consecutive requests.
    pub fn new(min_gap: Duration) -> Self {
        Self {
            sem: Semaphore::new(1),
            last: Mutex::new(None),
            min_gap,
        }
    }

    /// Acquire the gate, returning a permit that releases when dropped. Sleeps
    /// until the configured `min_gap` has elapsed since the previous call.
    pub async fn acquire(&self) -> ArxivPermit<'_> {
        let permit = self
            .sem
            .acquire()
            .await
            .expect("arxiv semaphore should never close");

        // Enforce min spacing while holding the semaphore.
        {
            let mut last = self.last.lock().await;
            if let Some(t) = *last {
                let elapsed = t.elapsed();
                if elapsed < self.min_gap {
                    tokio::time::sleep(self.min_gap - elapsed).await;
                }
            }
            *last = Some(Instant::now());
        }

        ArxivPermit { _permit: permit }
    }
}

/// RAII guard returned by [`ArxivGate::acquire`].
pub struct ArxivPermit<'a> {
    _permit: tokio::sync::SemaphorePermit<'a>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn serializes_and_enforces_gap() {
        // Use a short gap so the test stays fast; we only want to prove the
        // gate serialises and waits.
        let gate = Arc::new(ArxivGate::new(Duration::from_millis(50)));
        let start = std::time::Instant::now();
        let g1 = gate.clone();
        let g2 = gate.clone();
        let t1 = tokio::spawn(async move {
            let _p = g1.acquire().await;
        });
        let t2 = tokio::spawn(async move {
            let _p = g2.acquire().await;
        });
        t1.await.unwrap();
        t2.await.unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(50),
            "expected at least 50ms gap, got {elapsed:?}"
        );
    }
}
