use crate::{
    backend::Backend,
    service::Service,
    types::{GenerateRequest, Job},
};
use anyhow::Result;
use async_trait::async_trait;
use std::{path::PathBuf, time::Duration};
use uuid::Uuid;

#[async_trait]
pub trait AgentApi: Send + Sync {
    async fn generate(&self, request: GenerateRequest) -> Result<Job>;
    async fn status(&self, id: Uuid, wait: Duration) -> Result<Job>;
    async fn list(&self) -> Vec<Job>;
    async fn download(&self, id: Uuid, output: Option<PathBuf>, wait: Duration) -> Result<PathBuf>;
    async fn retry(&self, id: Uuid) -> Result<Job>;
}

#[async_trait]
impl<B> AgentApi for Service<B>
where
    B: Backend + Send + 'static,
{
    async fn generate(&self, request: GenerateRequest) -> Result<Job> {
        self.submit(request).await
    }
    async fn status(&self, id: Uuid, wait: Duration) -> Result<Job> {
        Service::status(self, id, wait).await
    }
    async fn list(&self) -> Vec<Job> {
        Service::list(self).await
    }
    async fn download(&self, id: Uuid, output: Option<PathBuf>, wait: Duration) -> Result<PathBuf> {
        Service::download(self, id, output, wait).await
    }
    async fn retry(&self, id: Uuid) -> Result<Job> {
        Service::retry(self, id).await
    }
}
