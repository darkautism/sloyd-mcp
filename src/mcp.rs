use crate::api::AgentApi;
use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::{path::PathBuf, str::FromStr, sync::Arc};
use uuid::Uuid;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GenerateParams {
    #[schemars(
        description = "Optional reference image. Provide for Image-to-3D; omit for Text-to-3D."
    )]
    pub image: Option<PathBuf>,
    #[schemars(description = "Text-to-3D guidance. For Image-to-3D this is metadata only.")]
    pub prompt: Option<String>,
    #[schemars(
        description = "Exact low-poly face count: 3000,4000,5000,10000,20000,40000,100000."
    )]
    pub polycount: u32,
}
#[derive(Debug, Deserialize, JsonSchema)]
pub struct JobParams {
    pub id: String,
}
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DownloadParams {
    pub id: String,
    pub output: PathBuf,
}

#[derive(Clone)]
pub struct SloydMcp {
    api: Arc<dyn AgentApi>,
    tool_router: ToolRouter<Self>,
}
impl SloydMcp {
    pub fn new(api: Arc<dyn AgentApi>) -> Self {
        Self {
            api,
            tool_router: Self::tool_router(),
        }
    }
    fn id(s: &str) -> Result<Uuid, String> {
        Uuid::from_str(s).map_err(|e| e.to_string())
    }
}
#[tool_router(router=tool_router)]
impl SloydMcp {
    #[tool(
        name = "generate",
        description = "Generate a low-poly Sloyd asset using the authenticated Web session."
    )]
    async fn generate(&self, Parameters(p): Parameters<GenerateParams>) -> Result<String, String> {
        let j = self
            .api
            .generate(p.image, p.prompt.unwrap_or_default(), p.polycount)
            .await
            .map_err(|e| e.to_string())?;
        serde_json::to_string(&j).map_err(|e| e.to_string())
    }
    #[tool(name = "status", description = "Get current Sloyd generation state.")]
    async fn status(&self, Parameters(p): Parameters<JobParams>) -> Result<String, String> {
        serde_json::to_string(
            &self
                .api
                .status(Self::id(&p.id)?)
                .await
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }
    #[tool(
        name = "download",
        description = "Download completed asset as original GLB. Does not invoke Sloyd format conversion."
    )]
    async fn download(&self, Parameters(p): Parameters<DownloadParams>) -> Result<String, String> {
        self.api
            .download(Self::id(&p.id)?, &p.output)
            .await
            .map_err(|e| e.to_string())?;
        Ok(p.output.display().to_string())
    }
    #[tool(name = "retry", description = "Retry a Sloyd generation.")]
    async fn retry(&self, Parameters(p): Parameters<JobParams>) -> Result<String, String> {
        serde_json::to_string(
            &self
                .api
                .retry(Self::id(&p.id)?)
                .await
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }
}
#[tool_handler(router=self.tool_router)]
impl ServerHandler for SloydMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("Sloyd Web low-poly generation tools")
    }
}
