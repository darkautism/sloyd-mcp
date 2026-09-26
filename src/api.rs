use crate::{backend::Backend, service::Service, types::Job};
use anyhow::Result;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[async_trait]
pub trait AgentApi: Send + Sync {
    async fn generate(&self, image: Option<PathBuf>, prompt: String, polycount: u32)
    -> Result<Job>;
    async fn status(&self, id: Uuid) -> Result<Job>;
    async fn download(&self, id: Uuid, output: &Path) -> Result<()>;
    async fn retry(&self, id: Uuid) -> Result<Job>;
}

#[async_trait]
impl<B> AgentApi for Service<B>
where
    B: Backend + Send + 'static,
{
    async fn generate(
        &self,
        image: Option<PathBuf>,
        prompt: String,
        polycount: u32,
    ) -> Result<Job> {
        self.submit(crate::types::GenerateRequest {
            image,
            prompt,
            polycount,
        })
        .await
    }
    async fn status(&self, id: Uuid) -> Result<Job> {
        Service::status(self, id).await
    }
    async fn download(&self, id: Uuid, output: &Path) -> Result<()> {
        Service::download(self, id, output).await
    }
    async fn retry(&self, id: Uuid) -> Result<Job> {
        Service::retry(self, id).await
    }
}

use anyhow::{Context, Result, bail};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Debug)]
pub struct ChromiumProcess {
    child: Child,
    pub port: u16,
    #[allow(dead_code)]
    profile_dir: PathBuf,
}

impl ChromiumProcess {
    pub fn spawn(binary: &Path, profile_dir: &Path, port: u16) -> Result<Self> {
        if !binary.is_file() {
            bail!("Chromium not found: {}", binary.display());
        }
        std::fs::create_dir_all(profile_dir)?;
        let child = Command::new(binary)
            .arg("--headless=new")
            .arg("--no-sandbox")
            .arg("--disable-gpu")
            .arg("--disable-dev-shm-usage")
            .arg(format!("--remote-debugging-port={port}"))
            .arg(format!("--user-data-dir={}", profile_dir.display()))
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("spawn Chromium")?;
        Ok(Self {
            child,
            port,
            profile_dir: profile_dir.to_path_buf(),
        })
    }

    pub fn devtools_ready(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if std::net::TcpStream::connect(("127.0.0.1", self.port)).is_ok() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }
}

impl Drop for ChromiumProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

use anyhow::{Context, Result, bail};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Debug)]
pub struct LightpandaProcess {
    child: Child,
    pub port: u16,
    #[allow(dead_code)]
    cookie_file: PathBuf,
}

impl LightpandaProcess {
    pub fn spawn(binary: &Path, cookie_file: &Path, cache_dir: &Path, port: u16) -> Result<Self> {
        if !binary.is_file() {
            bail!("Lightpanda not found: {}", binary.display());
        }
        if let Some(parent) = cookie_file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::create_dir_all(cache_dir)?;
        let child = Command::new(binary)
            .arg("serve")
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(port.to_string())
            .arg("--cookie-file")
            .arg(cookie_file)
            .arg("--http-cache-dir")
            .arg(cache_dir)
            .arg("--load-resources")
            .arg("iframe")
            .arg("--log-filter")
            .arg("-all")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .context("spawn Lightpanda")?;
        Ok(Self {
            child,
            port,
            cookie_file: cookie_file.to_path_buf(),
        })
    }

    pub fn devtools_ready(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if std::net::TcpStream::connect(("127.0.0.1", self.port)).is_ok() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }
}

impl Drop for LightpandaProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub mod chromium;
pub mod lightpanda;
pub mod sloyd_cdp;

use crate::types::{GenerateRequest, JobStatus};
use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;

#[async_trait]
pub trait Backend {
    fn name(&self) -> &'static str;
    async fn health(&self) -> Result<()>;
    async fn submit(&mut self, request: &GenerateRequest) -> Result<String>;
    async fn status(&mut self, remote_id: &str) -> Result<JobStatus>;
    async fn download(&mut self, remote_id: &str, output: &Path) -> Result<()>;
    async fn retry(&mut self, remote_id: &str, request: &GenerateRequest) -> Result<String> {
        let _ = remote_id;
        self.submit(request).await
    }
}