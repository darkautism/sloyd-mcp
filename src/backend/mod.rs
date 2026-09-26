pub mod chromium;
pub mod lightpanda;
pub mod sloyd_cdp;
pub mod sloyd_http;

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
