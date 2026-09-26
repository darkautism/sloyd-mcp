/sloyd_cdp.rs
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
                    multipart::Form::new()
                        .part("file", part)
                        .text("options", "-lowpoly")
                        .text("targetFaceCount", r.polycount.to_string())
                        .text("textureResolution", "1k")
                        .text("topology", "triangles"),
                )
                .send()
                .await?
        } else {
            self.http.post(TEXT_JOB).headers(Self::map(h)?).json(&serde_json::json!({"prompt":r.prompt,"genStyleId":"Auto","options":"-lowpoly","targetFaceCount":r.polycount,"textureResolution":"1k","topology":"triangles","tPose":false})).send().await?
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

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NormalizedCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: Option<String>,
    pub secure: Option<bool>,
    #[serde(rename = "httpOnly")]
    pub http_only: Option<bool>,
    #[serde(rename = "sameSite")]
    pub same_site: Option<String>,
    pub expires: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct CookieImport {
    pub cookies: Vec<NormalizedCookie>,
    pub dropped_non_sloyd: usize,
}

pub async fn import_cookie_file(path: &Path) -> Result<CookieImport> {
    let text = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("read cookie export {}", path.display()))?;
    if text.trim_start().starts_with('[') || text.trim_start().starts_with('{') {
        parse_json(&text)
    } else {
        parse_netscape(&text)
    }
}

fn is_sloyd(domain: &str) -> bool {
    let d = domain.trim_start_matches('.').to_ascii_lowercase();
    d == "sloyd.ai" || d.ends_with(".sloyd.ai")
}

fn parse_json(text: &str) -> Result<CookieImport> {
    let root: Value = serde_json::from_str(text)?;
    let items = match &root {
        Value::Array(v) => v,
        Value::Object(o) => o
            .get("cookies")
            .and_then(Value::as_array)
            .context("cookie JSON must be array or {cookies:[...]}")?,
        _ => bail!("invalid cookie JSON"),
    };
    let mut cookies = Vec::new();
    let mut dropped = 0;
    for item in items {
        let Some(o) = item.as_object() else { continue };
        let Some(name) = o.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(value) = o.get("value").and_then(Value::as_str) else {
            continue;
        };
        let domain = o
            .get("domain")
            .or_else(|| o.get("host"))
            .and_then(Value::as_str);
        let Some(domain) = domain else { continue };
        if !is_sloyd(domain) {
            dropped += 1;
            continue;
        }
        cookies.push(NormalizedCookie {
            name: name.into(),
            value: value.into(),
            domain: domain.into(),
            path: o
                .get("path")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or(Some("/".into())),
            secure: o.get("secure").and_then(Value::as_bool),
            http_only: o.get("httpOnly").and_then(Value::as_bool),
            same_site: o.get("sameSite").and_then(Value::as_str).map(str::to_owned),
            expires: o
                .get("expires")
                .and_then(Value::as_f64)
                .or_else(|| o.get("expirationDate").and_then(Value::as_f64)),
        });
    }
    if cookies.is_empty() {
        bail!("cookie export contained no *.sloyd.ai cookies");
    }
    Ok(CookieImport {
        cookies,
        dropped_non_sloyd: dropped,
    })
}

fn parse_netscape(text: &str) -> Result<CookieImport> {
    let mut cookies = Vec::new();
    let mut dropped = 0;
    for raw in text.lines() {
        if raw.is_empty() || (raw.starts_with('#') && !raw.starts_with("#HttpOnly_")) {
            continue;
        }
        let (line, http_only) = raw
            .strip_prefix("#HttpOnly_")
            .map(|x| (x, true))
            .unwrap_or((raw, false));
        let f: Vec<&str> = line.splitn(7, '\t').collect();
        if f.len() != 7 {
            continue;
        }
        if !is_sloyd(f[0]) {
            dropped += 1;
            continue;
        }
        cookies.push(NormalizedCookie {
            name: f[5].into(),
            value: f[6].into(),
            domain: f[0].into(),
            path: Some(f[2].into()),
            secure: Some(f[3].eq_ignore_ascii_case("TRUE")),
            http_only: Some(http_only),
            same_site: None,
            expires: f[4].parse::<f64>().ok().filter(|v| *v > 0.0),
        });
    }
    if cookies.is_empty() {
        bail!("cookie export contained no *.sloyd.ai cookies");
    }
    Ok(CookieImport {
        cookies,
        dropped_non_sloyd: dropped,
    })
}

pub mod api;
pub mod backend;
pub mod cookies;
pub mod mcp;
pub mod service;
pub mod types;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use rmcp::ServiceExt;
use sloyd_mcp::{
    api::AgentApi,
    backend::{Backend, sloyd_cdp::SloydWebBackend},
    cookies::import_cookie_file,
    mcp::SloydMcp,
    service::Service,
};
use std::{path::PathBuf, sync::Arc};

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Engine {
    Auto,
    Lightpanda,
    Chromium,
}
#[derive(Parser, Debug)]
struct Cli {
    #[arg(long,value_enum,default_value_t=Engine::Auto)]
    engine: Engine,
    #[arg(long, default_value = "/root/sloyd-mcp/bin/lightpanda")]
    lightpanda: PathBuf,
    #[arg(long, default_value = "/usr/bin/chromium")]
    chromium: PathBuf,
    #[arg(long, default_value = "/root/sloyd-mcp/state/chromium")]
    profile: PathBuf,
    #[arg(long, default_value = "/root/sloyd-mcp/state/cookies.json")]
    cookies: PathBuf,
    #[arg(long, default_value = "/root/sloyd-mcp/state/cache")]
    cache: PathBuf,
    #[arg(long, default_value_t = 9232)]
    port: u16,
    #[arg(long, default_value_t = false)]
    allow_guest: bool,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand, Debug)]
enum Command {
    Doctor,
    SessionProbe,
    ImportCookies {
        source: PathBuf,
    },
    Mcp {
        #[arg(long, default_value_t = 2)]
        concurrency: usize,
    },
}
async fn light(cli: &Cli) -> Result<SloydWebBackend> {
    SloydWebBackend::launch_lightpanda(
        &cli.lightpanda,
        &cli.cookies,
        &cli.cache,
        cli.port,
        cli.allow_guest,
    )
    .await
}
async fn chrome(cli: &Cli) -> Result<SloydWebBackend> {
    SloydWebBackend::launch_chromium(
        &cli.chromium,
        &cli.profile,
        &cli.cookies,
        cli.port,
        cli.allow_guest,
    )
    .await
}
async fn backend(cli: &Cli) -> Result<SloydWebBackend> {
    match cli.engine {
        Engine::Lightpanda => light(cli).await,
        Engine::Chromium => chrome(cli).await,
        Engine::Auto => match light(cli).await {
            Ok(b) => Ok(b),
            Err(_) => chrome(cli).await,
        },
    }
}
#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match &cli.command {
        Command::Doctor => {
            println!("lightpanda={}", cli.lightpanda.is_file());
            println!("chromium={}", cli.chromium.is_file());
            println!("cookies={}", cli.cookies.display());
        }
        Command::SessionProbe => {
            let b = backend(&cli).await?;
            println!("authenticated={}", b.session_probe().await?);
        }
        Command::ImportCookies { source } => {
            let imported = import_cookie_file(source).await?;
            if let Some(p) = cli.cookies.parent() {
                tokio::fs::create_dir_all(p).await?;
            }
            tokio::fs::write(&cli.cookies, serde_json::to_vec_pretty(&imported.cookies)?).await?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                tokio::fs::set_permissions(&cli.cookies, std::fs::Permissions::from_mode(0o600))
                    .await?;
            }
            println!("imported_sloyd_cookies={}", imported.cookies.len());
            println!("dropped_non_sloyd={}", imported.dropped_non_sloyd);
        }
        Command::Mcp { concurrency } => {
            let b = backend(&cli).await?;
            b.health().await?;
            let api: Arc<dyn AgentApi> = Arc::new(Service::new(b, *concurrency)?);
            SloydMcp::new(api)
                .serve(rmcp::transport::stdio())
                .await?
                .waiting()
                .await?;
        }
    }
    Ok(())
}