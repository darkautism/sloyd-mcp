use crate::{
    backend::Backend,
    session::Auth0Session,
    types::{GenerateRequest, JobStatus},
};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use reqwest::{
    Client, StatusCode,
    header::{AUTHORIZATION, HeaderMap, HeaderValue, ORIGIN, REFERER, USER_AGENT},
    multipart,
};
use std::{path::Path, time::Duration};

const IMAGE_JOB: &str = "https://api.sloyd.ai/api/jobs/image-to-3d";
const TEXT_JOB: &str = "https://api.sloyd.ai/api/jobs/text-to-3d";
const API: &str = "https://api.sloyd.ai/api";
const GCS: &str = "https://storage.googleapis.com/ai-services-quality";

pub struct SloydHttpBackend {
    auth: Auth0Session,
    http: Client,
}

impl SloydHttpBackend {
    pub fn new(cookie_path: impl Into<std::path::PathBuf>) -> Result<Self> {
        Ok(Self {
            auth: Auth0Session::new(cookie_path)?,
            http: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .context("build Sloyd HTTP client")?,
        })
    }

    fn challenge(body: &str) -> bool {
        let body = body.to_ascii_lowercase();
        [
            "turnstile",
            "cf-turnstile",
            "captcha",
            "challenges.cloudflare.com",
            "x-turnstile-token",
        ]
        .iter()
        .any(|needle| body.contains(needle))
    }

    fn job_id(value: &serde_json::Value) -> Option<String> {
        value
            .get("jobId")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                value
                    .get("data")
                    .and_then(|data| data.get("jobId"))
                    .and_then(serde_json::Value::as_str)
            })
            .or_else(|| value.get("data").and_then(serde_json::Value::as_str))
            .map(ToOwned::to_owned)
    }

    fn glb_url(job_id: &str) -> String {
        format!("{GCS}/jobs/{job_id}.glb")
    }

    async fn headers(&mut self) -> Result<HeaderMap> {
        let token = self.auth.access_token().await?;
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))
                .context("invalid bearer token header")?,
        );
        headers.insert(ORIGIN, HeaderValue::from_static("https://app.sloyd.ai"));
        headers.insert(REFERER, HeaderValue::from_static("https://app.sloyd.ai/"));
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static(
                "Mozilla/5.0 (compatible; sloyd-mcp/0.1; +https://github.com/darkautism/sloyd-mcp)",
            ),
        );
        Ok(headers)
    }

    async fn submit_once(&mut self, request: &GenerateRequest) -> Result<(StatusCode, String)> {
        let headers = self.headers().await?;

        let response = if let Some(image) = &request.image {
            let bytes = tokio::fs::read(image)
                .await
                .with_context(|| format!("read image {}", image.display()))?;
            let filename = image
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("reference.jpg")
                .to_owned();

            let part = multipart::Part::bytes(bytes).file_name(filename);
            self.http
                .post(IMAGE_JOB)
                .headers(headers)
                .multipart(
                    multipart::Form::new()
                        .part("file", part)
                        .text("options", "-lowpoly")
                        .text("targetFaceCount", request.polycount.to_string())
                        .text("textureResolution", "1k")
                        .text("topology", "triangles"),
                )
                .send()
                .await
                .context("submit Sloyd Image-to-3D job")?
        } else {
            self.http
                .post(TEXT_JOB)
                .headers(headers)
                .json(&serde_json::json!({
                    "prompt": request.prompt,
                    "genStyleId": "Auto",
                    "options": "-lowpoly",
                    "targetFaceCount": request.polycount,
                    "textureResolution": "1k",
                    "topology": "triangles",
                    "tPose": false
                }))
                .send()
                .await
                .context("submit Sloyd Text-to-3D job")?
        };

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        Ok((status, body))
    }

    async fn submit_job(&mut self, request: &GenerateRequest) -> Result<String> {
        for attempt in 0..2 {
            let (status, body) = self.submit_once(request).await?;

            if Self::challenge(&body) {
                bail!(
                    "Sloyd requires interactive verification; automation stopped before retrying"
                );
            }

            if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) && attempt == 0 {
                self.auth.invalidate();
                continue;
            }

            if !status.is_success() {
                bail!(
                    "Sloyd generation failed HTTP {}: {}",
                    status,
                    body.chars().take(500).collect::<String>()
                );
            }

            let body: serde_json::Value =
                serde_json::from_str(&body).context("parse Sloyd generation response")?;
            return Self::job_id(&body).context("Sloyd generation response contained no jobId");
        }

        bail!("Sloyd authorization could not be refreshed")
    }

    async fn status_once(&mut self, job_id: &str) -> Result<(StatusCode, String)> {
        let response = self
            .http
            .get(format!("{API}/jobs/{job_id}"))
            .headers(self.headers().await?)
            .send()
            .await
            .context("query Sloyd job status")?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        Ok((status, body))
    }

    async fn probe_status(&mut self, job_id: &str) -> Result<JobStatus> {
        let glb = self
            .http
            .head(Self::glb_url(job_id))
            .send()
            .await
            .context("probe Sloyd GLB object")?;
        if glb.status().is_success() {
            return Ok(JobStatus::Completed);
        }

        for attempt in 0..2 {
            let (status, body) = self.status_once(job_id).await?;

            if Self::challenge(&body) {
                bail!("Sloyd job status requires interactive verification");
            }

            if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) && attempt == 0 {
                self.auth.invalidate();
                continue;
            }

            if status == StatusCode::NOT_FOUND {
                return Ok(JobStatus::Running);
            }

            if !status.is_success() {
                bail!(
                    "Sloyd job status failed HTTP {}: {}",
                    status,
                    body.chars().take(500).collect::<String>()
                );
            }

            let value: serde_json::Value =
                serde_json::from_str(&body).context("parse Sloyd job status response")?;
            let data = value.get("data").unwrap_or(&value);
            let status = data
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_ascii_lowercase();

            if matches!(
                status.as_str(),
                "failed" | "error" | "cancelled" | "canceled" | "rejected"
            ) {
                let message = data
                    .get("errorMessage")
                    .or_else(|| data.get("error"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("Sloyd reported generation failure");
                return Ok(JobStatus::Failed {
                    message: message.to_owned(),
                });
            }

            return Ok(JobStatus::Running);
        }

        bail!("Sloyd authorization could not be refreshed")
    }

    async fn fetch_glb(&self, job_id: &str) -> Result<Vec<u8>> {
        let response = self
            .http
            .get(Self::glb_url(job_id))
            .send()
            .await
            .context("download Sloyd GLB")?;

        if !response.status().is_success() {
            bail!("Sloyd GLB download failed HTTP {}", response.status());
        }

        let bytes = response.bytes().await?.to_vec();
        if bytes.len() < 4 || &bytes[..4] != b"glTF" {
            bail!("downloaded response is not a GLB payload");
        }
        Ok(bytes)
    }

    pub async fn session_probe(&mut self) -> Result<bool> {
        self.auth.probe().await
    }
}

#[async_trait]
impl Backend for SloydHttpBackend {
    fn name(&self) -> &'static str {
        "sloyd-http"
    }

    async fn health(&self) -> Result<()> {
        Ok(())
    }

    async fn submit(&mut self, request: &GenerateRequest) -> Result<String> {
        self.submit_job(request).await
    }

    async fn status(&mut self, remote_id: &str) -> Result<JobStatus> {
        self.probe_status(remote_id).await
    }

    async fn download(&mut self, remote_id: &str, output: &Path) -> Result<()> {
        let bytes = self.fetch_glb(remote_id).await?;
        if let Some(parent) = output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(output, bytes)
            .await
            .with_context(|| format!("write GLB {}", output.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glb_url_is_public_object_path() {
        assert_eq!(
            SloydHttpBackend::glb_url("abc123"),
            "https://storage.googleapis.com/ai-services-quality/jobs/abc123.glb"
        );
    }

    #[test]
    fn detects_interactive_verification() {
        assert!(SloydHttpBackend::challenge(
            r#"{"error":"Turnstile verification failed"}"#
        ));
        assert!(!SloydHttpBackend::challenge(
            r#"{"error":"Out of trial previews"}"#
        ));
    }
}
