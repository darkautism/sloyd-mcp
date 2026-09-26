use crate::{
    backend::{Backend, chromium::ChromiumProcess, lightpanda::LightpandaProcess},
    types::{GenerateRequest, JobStatus},
};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use chromiumoxide::{
    Browser, Page,
    cdp::browser_protocol::{
        fetch::{
            ContinueRequestParams, DisableParams as FetchDisableParams,
            EnableParams as FetchEnableParams, EventRequestPaused, RequestPattern, RequestStage,
        },
        network::CookieParam,
    },
};
use futures_util::StreamExt;
use reqwest::{
    Client, StatusCode,
    header::{HeaderMap, HeaderName, HeaderValue},
    multipart,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::task::JoinHandle;

const PAGE: &str = "https://app.sloyd.ai/create-3d?mode=image";
const IMAGE_JOB: &str = "https://api.sloyd.ai/api/jobs/image-to-3d";
const TEXT_JOB: &str = "https://api.sloyd.ai/api/jobs/text-to-3d";
const API: &str = "https://api.sloyd.ai/api";
const GCS: &str = "https://storage.googleapis.com/ai-services-quality";

#[allow(dead_code)]
enum EngineProcess {
    Chromium(ChromiumProcess),
    Lightpanda(LightpandaProcess),
}

pub struct SloydWebBackend {
    _process: EngineProcess,
    browser: Browser,
    _handler: JoinHandle<()>,
    allow_guest: bool,
    http: Client,
    headers: Option<BTreeMap<String, String>>,
}
impl SloydWebBackend {
    async fn connect(
        process: EngineProcess,
        url: String,
        name: &str,
        allow_guest: bool,
    ) -> Result<Self> {
        let (browser, mut handler) = Browser::connect(url)
            .await
            .with_context(|| format!("connect to {name}"))?;
        let task = tokio::spawn(async move {
            while let Some(e) = handler.next().await {
                if e.is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            _process: process,
            browser,
            _handler: task,
            allow_guest,
            http: Client::builder().timeout(Duration::from_secs(30)).build()?,
            headers: None,
        })
    }
    pub async fn launch_lightpanda(
        bin: &Path,
        cookies: &Path,
        cache: &Path,
        port: u16,
        allow_guest: bool,
    ) -> Result<Self> {
        let p = LightpandaProcess::spawn(bin, cookies, cache, port)?;
        if !p.devtools_ready(Duration::from_secs(10)) {
            bail!("Lightpanda not ready")
        }
        Self::connect(
            EngineProcess::Lightpanda(p),
            format!("ws://127.0.0.1:{port}/"),
            "Lightpanda",
            allow_guest,
        )
        .await
    }
    pub async fn launch_chromium(
        bin: &Path,
        profile: &Path,
        cookies: &Path,
        port: u16,
        allow_guest: bool,
    ) -> Result<Self> {
        let p = ChromiumProcess::spawn(bin, profile, port)?;
        if !p.devtools_ready(Duration::from_secs(10)) {
            bail!("Chromium not ready")
        }
        let b = Self::connect(
            EngineProcess::Chromium(p),
            format!("http://127.0.0.1:{port}"),
            "Chromium",
            allow_guest,
        )
        .await?;
        if cookies.is_file() {
            let bytes = tokio::fs::read(cookies).await?;
            let c: Vec<CookieParam> = serde_json::from_slice(&bytes)?;
            if !c.is_empty() {
                b.browser.set_cookies(c).await?;
            }
        }
        Ok(b)
    }
    async fn capture(page: &Page, require_auth: bool) -> Result<BTreeMap<String, String>> {
        let pattern = RequestPattern::builder()
            .url_pattern("*api.sloyd.ai/api/*")
            .request_stage(RequestStage::Request)
            .build();
        page.execute(FetchEnableParams::builder().pattern(pattern).build())
            .await?;
        let mut paused = page.event_listener::<EventRequestPaused>().await?;
        let out = async {
            page.goto(PAGE).await?;
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                let remain = deadline.saturating_duration_since(Instant::now());
                if remain.is_zero() {
                    bail!("timed out waiting for Sloyd API headers")
                }
                let ev = tokio::time::timeout(remain.min(Duration::from_secs(5)), paused.next())
                    .await
                    .context("timed out waiting for Sloyd API request")?
                    .context("fetch stream closed")?;
                let mut h = BTreeMap::new();
                if let Some(o) = ev.request.headers.inner().as_object() {
                    for (n, v) in o {
                        let l = n.to_ascii_lowercase();
                        if matches!(
                            l.as_str(),
                            "authorization"
                                | "x-device-id"
                                | "x-is-mobile"
                                | "x-mixpanel-id"
                                | "origin"
                                | "referer"
                                | "user-agent"
                        ) && let Some(s) = v.as_str()
                        {
                            h.insert(l, s.to_owned());
                        }
                    }
                }
                page.execute(ContinueRequestParams::new(ev.request_id.clone()))
                    .await?;
                if (!require_auth && !h.is_empty()) || h.contains_key("authorization") {
                    h.entry("origin".into())
                        .or_insert_with(|| "https://app.sloyd.ai".into());
                    h.entry("referer".into())
                        .or_insert_with(|| "https://app.sloyd.ai/".into());
                    break Ok(h);
                }
            }
        }
        .await;
        let _ = page.execute(FetchDisableParams::default()).await;
        out
    }
    async fn web_headers(&mut self) -> Result<BTreeMap<String, String>> {
        if let Some(h) = &self.headers {
            return Ok(h.clone());
        }
        let p = self.browser.new_page("about:blank").await?;
        let r = Self::capture(&p, !self.allow_guest).await;
        let _ = p.close().await;
        let h = r?;
        self.headers = Some(h.clone());
        Ok(h)
    }
    fn map(h: &BTreeMap<String, String>) -> Result<HeaderMap> {
        let mut out = HeaderMap::new();
        for (n, v) in h {
            out.insert(
                HeaderName::from_bytes(n.as_bytes())?,
                HeaderValue::from_str(v)?,
            );
        }
        Ok(out)
    }
    fn challenge(body: &str) -> bool {
        let b = body.to_ascii_lowercase();
        [
            "turnstile",
            "captcha",
            "challenges.cloudflare.com",
            "x-turnstile-token",
        ]
        .iter()
        .any(|x| b.contains(x))
    }
    fn job_id(v: &serde_json::Value) -> Option<String> {
        v.get("jobId")
            .and_then(|x| x.as_str())
            .or_else(|| {
                v.get("data")
                    .and_then(|d| d.get("jobId"))
                    .and_then(|x| x.as_str())
            })
            .or_else(|| v.get("data").and_then(|x| x.as_str()))
            .map(str::to_owned)
    }
    fn glb(id: &str) -> String {
        format!("{GCS}/jobs/{id}.glb")
    }
    async fn submit_once(
        &self,
        r: &GenerateRequest,
        h: &BTreeMap<String, String>,
    ) -> Result<(StatusCode, String)> {
        let resp = if let Some(img) = &r.image {
            let bytes = tokio::fs::read(img).await?;
            let part = multipart::Part::bytes(bytes).file_name(
                img.file_name()
                    .and_then(|x| x.to_str())
                    .unwrap_or("reference.jpg")
                    .to_string(),
            );
            self.http
                .post(IMAGE_JOB)
                .headers(Self::map(h)?)
                .multipart(
                    r.image_fields()
                        .into_iter()
                        .fold(multipart::Form::new().part("file", part), |f, (k, v)| {
                            f.text(k, v)
                        }),
                )
                .send()
                .await?
        } else {
            self.http
                .post(TEXT_JOB)
                .headers(Self::map(h)?)
                .json(&r.text_body())
                .send()
                .await?
        };
        let s = resp.status();
        let b = resp.text().await.unwrap_or_default();
        Ok((s, b))
    }
    async fn submit_http(&mut self, r: &GenerateRequest) -> Result<String> {
        for attempt in 0..2 {
            let h = self.web_headers().await?;
            let (s, b) = self.submit_once(r, &h).await?;
            if Self::challenge(&b) {
                bail!("Sloyd requires interactive verification; stopped before retry")
            }
            if matches!(s, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
                && attempt == 0
                && !self.allow_guest
            {
                self.headers = None;
                continue;
            }
            if !s.is_success() {
                bail!(
                    "Sloyd generation failed HTTP {}: {}",
                    s,
                    b.chars().take(500).collect::<String>()
                )
            }
            return Self::job_id(&serde_json::from_str(&b)?).context("no jobId");
        }
        bail!("could not refresh Sloyd session")
    }
    async fn probe(&mut self, id: &str) -> Result<JobStatus> {
        let head = self.http.head(Self::glb(id)).send().await?;
        if head.status().is_success() {
            return Ok(JobStatus::Completed);
        }
        let h = match self.web_headers().await {
            Ok(x) => x,
            Err(_) => return Ok(JobStatus::Running),
        };
        let resp = self
            .http
            .get(format!("{API}/jobs/{id}"))
            .headers(Self::map(&h)?)
            .send()
            .await?;
        let s = resp.status();
        let b = resp.text().await.unwrap_or_default();
        if Self::challenge(&b) {
            bail!("Sloyd status requires interactive verification")
        }
        if s == StatusCode::NOT_FOUND {
            return Ok(JobStatus::Running);
        }
        if !s.is_success() {
            return Ok(JobStatus::Running);
        }
        let v: serde_json::Value = serde_json::from_str(&b)?;
        let d = v.get("data").unwrap_or(&v);
        let st = d
            .get("status")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(
            st.as_str(),
            "failed" | "error" | "cancelled" | "canceled" | "rejected"
        ) {
            return Ok(JobStatus::Failed {
                message: d
                    .get("errorMessage")
                    .or_else(|| d.get("error"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("Sloyd generation failed")
                    .into(),
            });
        }
        Ok(JobStatus::Running)
    }
    pub async fn session_probe(&self) -> Result<bool> {
        let p = self.browser.new_page(PAGE).await?;
        tokio::time::sleep(Duration::from_secs(2)).await;
        let v:serde_json::Value=p.evaluate(r#"document.querySelector('[data-testid="PageCreate3d-wrapper"]')?.dataset?.guest || null"#).await?.into_value()?;
        let _ = p.close().await;
        Ok(v.as_str() != Some("true"))
    }
}
#[async_trait]
impl Backend for SloydWebBackend {
    fn name(&self) -> &'static str {
        "sloyd-web"
    }
    async fn health(&self) -> Result<()> {
        Ok(())
    }
    async fn submit(&mut self, r: &GenerateRequest) -> Result<String> {
        self.submit_http(r).await
    }
    async fn status(&mut self, id: &str) -> Result<JobStatus> {
        self.probe(id).await
    }
    async fn download(&mut self, id: &str, out: &Path) -> Result<()> {
        let resp = self.http.get(Self::glb(id)).send().await?;
        if !resp.status().is_success() {
            bail!("GLB download HTTP {}", resp.status())
        }
        let b = resp.bytes().await?;
        if b.len() < 4 || &b[..4] != b"glTF" {
            bail!("not GLB")
        }
        if let Some(p) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
            tokio::fs::create_dir_all(p).await?;
        }
        tokio::fs::write(out, &b).await?;
        Ok(())
    }
}
pub fn default_profile(root: &Path) -> PathBuf {
    root.join("state/chromium")
}
