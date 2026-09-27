use crate::{
    api::AgentApi,
    types::{
        GenerateRequest, Job, JobStatus, License, Overrides, Preset, Style, Texture, Topology,
    },
};
use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, str::FromStr, sync::Arc, time::Duration};
use uuid::Uuid;

const INSTRUCTIONS: &str = "\
Sloyd AI 3D model generation (GLB output).
Workflow: `generate` -> `download` (download waits for the job to finish, ~2-3 min) -> done.
Start several `generate` calls before downloading to run jobs in parallel.
Pick the look with `preset` (defaults to game-lowpoly) and, for text prompts, `style`.
Describe only the object in `prompt`; polycount/topology/texture come from the preset, \
so do not put words like 'low poly' in the prompt.
Job ids live only as long as this server process; use `list` to recover them.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GenerateParams {
    #[schemars(
        description = "What to model, e.g. \"wooden chair with a round seat\". Required unless `image` is given. Describe the object only; the preset controls mesh density."
    )]
    pub prompt: Option<String>,
    #[schemars(
        description = "Path to a local reference image (png/jpg/webp) for Image-to-3D. When set, `prompt` and `style` are ignored."
    )]
    pub image: Option<PathBuf>,
    #[schemars(
        description = "Short label used for the default download filename. Defaults to the prompt."
    )]
    pub name: Option<String>,
    #[schemars(
        description = "Target use; sets default polycount, texture and topology. Default: game-lowpoly (10k faces, 1k texture, triangles)."
    )]
    pub preset: Option<Preset>,
    #[schemars(description = "Art style for Text-to-3D. Default: Auto (no style).")]
    pub style: Option<Style>,
    #[schemars(
        description = "Override the preset's face count. One of 3000, 4000, 5000, 10000, 20000, 40000, 100000, 200000, 500000 (500000 needs a subscription)."
    )]
    pub polycount: Option<u32>,
    #[schemars(description = "Override the preset's mesh topology.")]
    pub topology: Option<Topology>,
    #[schemars(description = "Override the preset's texture resolution; \"none\" for untextured.")]
    pub texture: Option<Texture>,
    #[schemars(
        description = "Force characters into a T-pose (useful for rigging). Default false."
    )]
    pub t_pose: Option<bool>,
    #[schemars(
        description = "Image-to-3D only: let Sloyd clean up/enhance the reference image first. Default false."
    )]
    pub refine: Option<bool>,
    #[schemars(description = "Model license. Default private.")]
    pub license: Option<License>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StatusParams {
    #[schemars(description = "Job id returned by `generate`.")]
    pub id: String,
    #[schemars(
        description = "Seconds to wait for the job to finish before answering (0-300). Default 0 = return immediately."
    )]
    pub wait_seconds: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct JobParams {
    #[schemars(description = "Job id returned by `generate`.")]
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DownloadParams {
    #[schemars(description = "Job id returned by `generate`.")]
    pub id: String,
    #[schemars(
        description = "Where to write the .glb. Default: downloads/<name>-<id prefix>.glb under the server's working directory."
    )]
    pub output: Option<PathBuf>,
    #[schemars(
        description = "Seconds to wait for the job to finish before giving up (0-600). Default 300."
    )]
    pub wait_seconds: Option<u64>,
}

/// Flat, self-describing job view returned by every tool.
#[derive(Serialize)]
struct JobView {
    id: Uuid,
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    elapsed_seconds: u64,
    settings: GenerateRequest,
    next: String,
}

impl From<Job> for JobView {
    fn from(j: Job) -> Self {
        let (state, error, next) = match j.status {
            JobStatus::Queued => (
                "queued",
                None,
                "waiting for a free slot; call `download` to wait and save it".to_owned(),
            ),
            JobStatus::Running => (
                "running",
                None,
                "usually takes 2-3 minutes; call `download` to wait and save it".to_owned(),
            ),
            JobStatus::Completed => (
                "completed",
                None,
                "call `download` to save the .glb".to_owned(),
            ),
            JobStatus::Failed { message } => (
                "failed",
                Some(message),
                "call `retry` to resubmit with the same settings".to_owned(),
            ),
        };
        Self {
            id: j.id,
            state,
            error,
            elapsed_seconds: j.elapsed_secs,
            settings: j.request,
            next,
        }
    }
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
        Uuid::from_str(s.trim()).map_err(|e| format!("invalid job id {s:?}: {e}"))
    }
    fn json<T: Serialize>(v: &T) -> Result<String, String> {
        serde_json::to_string_pretty(v).map_err(|e| e.to_string())
    }
}
#[tool_router(router=tool_router)]
impl SloydMcp {
    #[tool(
        name = "generate",
        description = "Start a Sloyd 3D model generation and return immediately with a job id. Text-to-3D: pass `prompt`. Image-to-3D: pass `image`. All other settings are optional; `preset` (default game-lowpoly) picks sensible polycount/texture/topology. Call several times to run jobs in parallel, then `download` each id."
    )]
    async fn generate(&self, Parameters(p): Parameters<GenerateParams>) -> Result<String, String> {
        let request = GenerateRequest::resolve(
            p.name,
            p.image,
            p.prompt.unwrap_or_default(),
            p.preset.unwrap_or_default(),
            p.style.unwrap_or_default(),
            Overrides {
                polycount: p.polycount,
                topology: p.topology,
                texture: p.texture,
            },
            p.t_pose.unwrap_or(false),
            p.refine.unwrap_or(false),
            p.license.unwrap_or_default(),
        );
        let job = self
            .api
            .generate(request)
            .await
            .map_err(|e| e.to_string())?;
        Self::json(&JobView::from(job))
    }

    #[tool(
        name = "status",
        description = "Get a job's state (queued/running/completed/failed) and its settings. Optionally wait up to `wait_seconds` for it to finish. Prefer `download`, which waits for you."
    )]
    async fn status(&self, Parameters(p): Parameters<StatusParams>) -> Result<String, String> {
        let wait = Duration::from_secs(p.wait_seconds.unwrap_or(0).min(300));
        let job = self
            .api
            .status(Self::id(&p.id)?, wait)
            .await
            .map_err(|e| e.to_string())?;
        Self::json(&JobView::from(job))
    }

    #[tool(
        name = "list",
        description = "List every job submitted to this server session, newest first, with state and settings."
    )]
    async fn list(&self) -> Result<String, String> {
        let jobs: Vec<JobView> = self.api.list().await.into_iter().map(Into::into).collect();
        Self::json(&jobs)
    }

    #[tool(
        name = "download",
        description = "Wait for a job to finish (up to `wait_seconds`, default 300) and save the original GLB. Returns the absolute file path. If it times out, call again to keep waiting."
    )]
    async fn download(&self, Parameters(p): Parameters<DownloadParams>) -> Result<String, String> {
        let wait = Duration::from_secs(p.wait_seconds.unwrap_or(300).min(600));
        let path = self
            .api
            .download(Self::id(&p.id)?, p.output, wait)
            .await
            .map_err(|e| e.to_string())?;
        Self::json(&serde_json::json!({ "id": p.id, "path": path }))
    }

    #[tool(
        name = "retry",
        description = "Resubmit a job (typically a failed one) with identical settings. Returns a NEW job id."
    )]
    async fn retry(&self, Parameters(p): Parameters<JobParams>) -> Result<String, String> {
        let job = self
            .api
            .retry(Self::id(&p.id)?)
            .await
            .map_err(|e| e.to_string())?;
        Self::json(&JobView::from(job))
    }
}
#[tool_handler(router=self.tool_router)]
impl ServerHandler for SloydMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(INSTRUCTIONS)
    }
}
