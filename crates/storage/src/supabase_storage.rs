//! Tier 2 — Supabase Object Storage REST wrapper.
//!
//! Uses the service-role key for writes. Bucket selection is up to the caller
//! — there is no implicit default bucket. The artifact-routing table lives in
//! [`crate::paper_artifacts`].

use anyhow::{anyhow, Context, Result};
use reqwest::{header, Client};
use serde::Deserialize;
use tracing::debug;

pub struct SupabaseStorage {
    pub url: String,
    pub service_role_key: String,
    client: Client,
}

impl SupabaseStorage {
    pub fn new(url: impl Into<String>, service_role_key: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            service_role_key: service_role_key.into(),
            client: Client::new(),
        }
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.service_role_key)
    }

    /// `POST /storage/v1/object/<bucket>/<key>`. Uses `x-upsert: true` so
    /// retries are idempotent.
    pub async fn put_object(
        &self,
        bucket: &str,
        key: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> Result<()> {
        let path = format!(
            "{}/storage/v1/object/{}/{}",
            self.url.trim_end_matches('/'),
            bucket,
            key.trim_start_matches('/')
        );
        debug!(%path, content_type, "putting object");
        let resp = self
            .client
            .post(&path)
            .header(header::AUTHORIZATION, self.auth_header())
            .header(header::CONTENT_TYPE, content_type)
            .header("x-upsert", "true")
            .body(bytes)
            .send()
            .await
            .with_context(|| format!("PUT {path}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("supabase storage put {path}: {status} {body}"));
        }
        Ok(())
    }

    /// `POST /storage/v1/object/sign/<bucket>/<key>` → presigned URL.
    pub async fn get_object_presigned(
        &self,
        bucket: &str,
        key: &str,
        ttl_secs: u64,
    ) -> Result<String> {
        #[derive(Deserialize)]
        struct SignResp {
            #[serde(rename = "signedURL")]
            signed_url: Option<String>,
            #[serde(rename = "signedUrl")]
            signed_url_alt: Option<String>,
        }
        let path = format!(
            "{}/storage/v1/object/sign/{}/{}",
            self.url.trim_end_matches('/'),
            bucket,
            key.trim_start_matches('/')
        );
        let resp = self
            .client
            .post(&path)
            .header(header::AUTHORIZATION, self.auth_header())
            .header(header::CONTENT_TYPE, "application/json")
            .body(format!("{{\"expiresIn\":{ttl_secs}}}"))
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("supabase storage sign {path}: {status} {body}"));
        }
        let parsed: SignResp = resp.json().await.context("parsing sign response")?;
        let url_path = parsed
            .signed_url
            .or(parsed.signed_url_alt)
            .ok_or_else(|| anyhow!("supabase storage sign returned no URL"))?;
        let base = self.url.trim_end_matches('/');
        let abs = if url_path.starts_with("http") {
            url_path
        } else if url_path.starts_with('/') {
            format!("{base}{url_path}")
        } else {
            format!("{base}/{url_path}")
        };
        Ok(abs)
    }

    /// List objects under `prefix` in `bucket`.
    pub async fn list_objects(&self, bucket: &str, prefix: &str) -> Result<Vec<String>> {
        #[derive(Deserialize)]
        struct Obj {
            name: String,
        }
        let path = format!(
            "{}/storage/v1/object/list/{}",
            self.url.trim_end_matches('/'),
            bucket
        );
        let body = serde_json::json!({
            "prefix": prefix.trim_matches('/'),
            "limit": 1000,
        });
        let resp = self
            .client
            .post(&path)
            .header(header::AUTHORIZATION, self.auth_header())
            .header(header::CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("supabase storage list {path}: {status} {body}"));
        }
        let items: Vec<Obj> = resp.json().await.context("parsing list response")?;
        Ok(items.into_iter().map(|o| o.name).collect())
    }

    /// Delete a single object: `DELETE /storage/v1/object/<bucket>/<key>`.
    pub async fn delete_object(&self, bucket: &str, key: &str) -> Result<()> {
        let path = format!(
            "{}/storage/v1/object/{}/{}",
            self.url.trim_end_matches('/'),
            bucket,
            key.trim_start_matches('/')
        );
        let resp = self
            .client
            .delete(&path)
            .header(header::AUTHORIZATION, self.auth_header())
            .send()
            .await
            .with_context(|| format!("DELETE {path}"))?;
        if !resp.status().is_success() && resp.status().as_u16() != 404 {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("supabase storage delete {path}: {status} {body}"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn wiremock_put_get_delete() -> Result<()> {
        let server = MockServer::start().await;
        let store = SupabaseStorage::new(server.uri(), "service-role-secret");

        Mock::given(method("POST"))
            .and(path("/storage/v1/object/raw-pdfs/2605.00403.pdf"))
            .and(header("authorization", "Bearer service-role-secret"))
            .and(header("content-type", "application/pdf"))
            .and(header("x-upsert", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{\"Key\":\"ok\"}"))
            .expect(1)
            .mount(&server)
            .await;
        store
            .put_object(
                "raw-pdfs",
                "2605.00403.pdf",
                b"%PDF-1.4".to_vec(),
                "application/pdf",
            )
            .await?;

        Mock::given(method("POST"))
            .and(path("/storage/v1/object/sign/raw-pdfs/2605.00403.pdf"))
            .and(header("authorization", "Bearer service-role-secret"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"signedURL":"/storage/v1/sign/abc?token=xyz"}"#),
            )
            .expect(1)
            .mount(&server)
            .await;
        let url = store
            .get_object_presigned("raw-pdfs", "2605.00403.pdf", 60)
            .await?;
        assert!(url.contains("/storage/v1/sign/abc"));

        Mock::given(method("POST"))
            .and(path("/storage/v1/object/list/extracted-json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(
                    r#"[{"name":"semantic_ast.json"},{"name":"figures/1.png"}]"#,
                ),
            )
            .expect(1)
            .mount(&server)
            .await;
        let names = store.list_objects("extracted-json", "2605.00403").await?;
        assert_eq!(names.len(), 2);

        Mock::given(method("DELETE"))
            .and(path("/storage/v1/object/raw-pdfs/2605.00403.pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&server)
            .await;
        store.delete_object("raw-pdfs", "2605.00403.pdf").await?;
        Ok(())
    }

    /// Live test against `supabase start` (Postgres + Storage at 127.0.0.1:54321).
    /// Run with `cargo test -p grokrxiv-storage -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn live_local_supabase_put_get_delete() -> Result<()> {
        let url = "http://127.0.0.1:54321";
        let key = std::env::var("SUPABASE_SERVICE_ROLE_KEY").map_err(|_| {
            anyhow!("set SUPABASE_SERVICE_ROLE_KEY to run live test (try `supabase status`)")
        })?;
        let store = SupabaseStorage::new(url, key);
        let bucket = "raw-pdfs";
        let obj = "_live_test_2605.99999.pdf";

        store
            .put_object(bucket, obj, b"%PDF-1.4 live".to_vec(), "application/pdf")
            .await?;
        let signed = store.get_object_presigned(bucket, obj, 60).await?;
        assert!(signed.starts_with("http"));
        store.delete_object(bucket, obj).await?;
        Ok(())
    }
}
