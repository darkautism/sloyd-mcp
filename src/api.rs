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
