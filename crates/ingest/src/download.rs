//! Network downloads for PDF + LaTeX source bundles.
//!
//! Per arXiv guidance we throttle to at most one request per 3 seconds via a
//! global semaphore. The `ARXIV_USER_AGENT` env var customises the UA string.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use bytes::Bytes;
use once_cell::sync::Lazy;
use tokio::sync::{Mutex, Semaphore};

/// 1 concurrent request at a time = enforces serial ordering for backoff.
static ARXIV_SEMAPHORE: Lazy<Arc<Semaphore>> = Lazy::new(|| Arc::new(Semaphore::new(1)));
/// Last request timestamp; used together with the semaphore to enforce ≥3s gap.
static LAST_REQUEST: Lazy<Mutex<Option<Instant>>> = Lazy::new(|| Mutex::new(None));

const MIN_GAP: Duration = Duration::from_secs(3);

fn user_agent() -> String {
    std::env::var("ARXIV_USER_AGENT").unwrap_or_else(|_| {
        "grokrxiv-ingest/0.1 (+https://grokrxiv.org; contact@grokrxiv.org)".to_string()
    })
}

fn http() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(user_agent())
        .timeout(Duration::from_secs(60))
        .build()
        .context("build reqwest client")
}

/// Perform a rate-limited GET against an arXiv endpoint with retry-on-transient-failure.
///
/// arXiv occasionally returns 503/429 under load (especially when multiple
/// orchestrator processes share the network — the intra-process semaphore
/// only serialises within ONE process, so 3 parallel `cargo run -- ingest`
/// invocations can all hit arXiv simultaneously). We retry up to 4 times with
/// exponential backoff: 2s, 5s, 12s, 30s. Total worst-case ~50s before
/// returning a hard error.
pub async fn rate_limited_get(url: &str) -> Result<Bytes> {
    let _permit = ARXIV_SEMAPHORE
        .clone()
        .acquire_owned()
        .await
        .context("acquire arxiv semaphore")?;

    // Enforce ≥ MIN_GAP between successive requests within this process.
    {
        let mut last = LAST_REQUEST.lock().await;
        if let Some(t) = *last {
            let elapsed = t.elapsed();
            if elapsed < MIN_GAP {
                tokio::time::sleep(MIN_GAP - elapsed).await;
            }
        }
        *last = Some(Instant::now());
    }

    let backoffs = [
        Duration::from_secs(2),
        Duration::from_secs(5),
        Duration::from_secs(12),
        Duration::from_secs(30),
    ];
    let mut last_err: Option<anyhow::Error> = None;
    for (attempt, wait) in std::iter::once(Duration::ZERO)
        .chain(backoffs.iter().copied())
        .enumerate()
    {
        if wait > Duration::ZERO {
            tracing::warn!(url, attempt, "arxiv transient failure; backing off");
            tokio::time::sleep(wait).await;
        }
        match http()?
            .get(url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))
        {
            Ok(resp) => match resp.error_for_status() {
                Ok(ok) => {
                    let bytes = ok
                        .bytes()
                        .await
                        .with_context(|| format!("body {url}"))?;
                    return Ok(bytes);
                }
                Err(e) => {
                    let status = e.status();
                    let transient = status
                        .map(|s| s.is_server_error() || s.as_u16() == 429)
                        .unwrap_or(false);
                    last_err = Some(anyhow::Error::new(e).context(format!("status {url}")));
                    if !transient {
                        break;
                    }
                }
            },
            Err(e) => {
                last_err = Some(e);
                // Treat all transport-level errors as transient.
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("arxiv get exhausted retries")))
}

/// Download a PDF from the supplied URL.
pub async fn download_pdf(url: &str) -> Result<Bytes> {
    rate_limited_get(url).await
}

/// Download the LaTeX source bundle (a `tar.gz`) from the supplied URL.
pub async fn download_source(url: &str) -> Result<Bytes> {
    rate_limited_get(url).await
}
