//! Supabase Storage REST client.
//!
//! We use the documented `/storage/v1/object/<bucket>/<path>` endpoint with the
//! service-role bearer token. Buckets used by GrokRxiv:
//!
//! * `bundles/` — review zip artifacts (public read after moderation merge).
//! * `pdfs/` — **private** bucket holding upstream arXiv PDFs strictly for
//!   pipeline re-runs. We never expose a public PDF URL.
//! * `renders/` — rendered HTML/PDF artifacts.

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};

/// Supabase Storage client tied to one project.
pub struct SupabaseStorage {
    url: String,
    service_key: String,
    http: reqwest::Client,
}

impl SupabaseStorage {
    /// Construct a client. `url` is the project URL (no trailing slash).
    pub fn new(url: impl Into<String>, service_key: impl Into<String>) -> Self {
        Self::with_client(url, service_key, reqwest::Client::new())
    }

    /// Construct a client with a caller-supplied `reqwest::Client` (useful in
    /// tests for redirecting at a mock server).
    pub fn with_client(
        url: impl Into<String>,
        service_key: impl Into<String>,
        http: reqwest::Client,
    ) -> Self {
        Self {
            url: url.into(),
            service_key: service_key.into(),
            http,
        }
    }

    fn auth_headers(&self, content_type: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", self.service_key))
                .expect("service key is ascii"),
        );
        // Supabase recommends also sending the apikey header.
        h.insert(
            "apikey",
            HeaderValue::from_str(&self.service_key).expect("apikey is ascii"),
        );
        if let Some(ct) = content_type {
            h.insert(
                CONTENT_TYPE,
                HeaderValue::from_str(ct).expect("content-type ascii"),
            );
        }
        h
    }

    /// Upload bytes to `<bucket>/<path>`. Returns the canonical Supabase
    /// storage URL the row points at.
    pub async fn upload(
        &self,
        bucket: &str,
        path: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> Result<String> {
        let endpoint = format!("{}/storage/v1/object/{bucket}/{path}", self.url);
        let resp = self
            .http
            .post(&endpoint)
            .headers(self.auth_headers(Some(content_type)))
            .body(bytes)
            .send()
            .await
            .with_context(|| format!("supabase upload {bucket}/{path}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("supabase upload failed ({status}): {text}");
        }
        Ok(format!(
            "{}/storage/v1/object/public/{bucket}/{path}",
            self.url
        ))
    }

    /// Mint a time-limited signed URL for a private object.
    ///
    /// arXiv-compliance fence: **refuses** to sign URLs into the `pdfs` bucket
    /// (or anything named with that prefix). Those objects are upstream arXiv
    /// PDFs/LaTeX we are not licensed to redistribute. If callers need to
    /// re-process a paper, they should re-fetch from arXiv (subject to the
    /// rate-limit gate) rather than serve a stored copy.
    pub async fn signed_url(&self, bucket: &str, path: &str, ttl_secs: u64) -> Result<String> {
        if bucket == "pdfs" || bucket.starts_with("pdfs") {
            anyhow::bail!(
                "refusing to sign url into private `pdfs` bucket — arXiv source must not be re-served"
            );
        }
        let endpoint = format!("{}/storage/v1/object/sign/{bucket}/{path}", self.url);
        #[derive(Serialize)]
        struct Body {
            #[serde(rename = "expiresIn")]
            expires_in: u64,
        }
        let resp = self
            .http
            .post(&endpoint)
            .headers(self.auth_headers(Some("application/json")))
            .json(&Body {
                expires_in: ttl_secs,
            })
            .send()
            .await
            .with_context(|| format!("supabase sign {bucket}/{path}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("supabase sign failed ({status}): {text}");
        }
        #[derive(Deserialize)]
        struct SignedUrl {
            #[serde(rename = "signedURL")]
            signed_url: String,
        }
        let parsed: SignedUrl = serde_json::from_str(&text).context("parse signed-url response")?;
        Ok(format!("{}/storage/v1{}", self.url, parsed.signed_url))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn upload_constructs_correct_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/storage/v1/object/bundles/test/x.zip"))
            .and(header("authorization", "Bearer SECRET"))
            .and(header("apikey", "SECRET"))
            .and(header("content-type", "application/zip"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{\"Key\": \"x\"}"))
            .mount(&server)
            .await;
        let client = SupabaseStorage::new(server.uri(), "SECRET");
        let url = client
            .upload("bundles", "test/x.zip", vec![1, 2, 3], "application/zip")
            .await
            .expect("upload");
        assert!(url.contains("/storage/v1/object/public/bundles/test/x.zip"));
    }

    #[tokio::test]
    async fn signed_url_parses_response_for_public_bucket() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/storage/v1/object/sign/bundles/x.zip"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"signedURL":"/object/sign/bundles/x.zip?token=abc"}"#),
            )
            .mount(&server)
            .await;
        let client = SupabaseStorage::new(server.uri(), "SECRET");
        let url = client
            .signed_url("bundles", "x.zip", 600)
            .await
            .expect("signed");
        assert!(url.ends_with("?token=abc"));
    }

    #[tokio::test]
    async fn signed_url_refuses_pdfs_bucket() {
        let client = SupabaseStorage::new("http://unused", "SECRET");
        let err = client
            .signed_url("pdfs", "private/a.pdf", 600)
            .await
            .expect_err("should refuse pdfs bucket");
        assert!(err.to_string().contains("pdfs"));
    }
}
