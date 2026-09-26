use crate::{
    backend::Backend,
    types::{GenerateRequest, Job, JobStatus, POLYCOUNTS},
};
use anyhow::{Context, Result, bail};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, Semaphore};
use uuid::Uuid;

struct Entry {
    request: GenerateRequest,
    remote_id: Option<String>,
    status: JobStatus,
    created: Instant,
}

impl Entry {
    fn job(&self, id: Uuid) -> Job {
        Job {
            id,
            status: self.status.clone(),
            request: self.request.clone(),
            elapsed_secs: self.created.elapsed().as_secs(),
        }
    }
}

pub struct Service<B: Backend + Send + 'static> {
    backend: Arc<Mutex<B>>,
    slots: Arc<Semaphore>,
    jobs: Arc<Mutex<HashMap<Uuid, Entry>>>,
}

impl<B: Backend + Send + 'static> Service<B> {
    pub fn new(backend: B, concurrency: usize) -> Result<Self> {
        if !(1..=5).contains(&concurrency) {
            bail!("concurrency must be 1..=5");
        }
        Ok(Self {
            backend: Arc::new(Mutex::new(backend)),
            slots: Arc::new(Semaphore::new(concurrency)),
            jobs: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub async fn submit(&self, request: GenerateRequest) -> Result<Job> {
        validate(&request)?;
        let id = Uuid::new_v4();
        self.jobs.lock().await.insert(
            id,
            Entry {
                request: request.clone(),
                remote_id: None,
                status: JobStatus::Queued,
                created: Instant::now(),
            },
        );
        self.spawn(id, false);
        self.status(id, Duration::ZERO).await
    }

    /// Returns the job, first waiting up to `wait` for it to finish.
    pub async fn status(&self, id: Uuid, wait: Duration) -> Result<Job> {
        let deadline = Instant::now() + wait;
        loop {
            let job = {
                let j = self.jobs.lock().await;
                j.get(&id)
                    .with_context(|| format!("unknown job id {id}; call `list` to see known jobs"))?
                    .job(id)
            };
            if job.status.is_terminal() || Instant::now() >= deadline {
                return Ok(job);
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    pub async fn list(&self) -> Vec<Job> {
        let j = self.jobs.lock().await;
        let mut jobs: Vec<Job> = j.iter().map(|(id, e)| e.job(*id)).collect();
        jobs.sort_by_key(|x| std::cmp::Reverse(x.elapsed_secs));
        jobs
    }

    /// Waits up to `wait` for completion, then writes the GLB. Returns the absolute path.
    pub async fn download(
        &self,
        id: Uuid,
        out: Option<PathBuf>,
        wait: Duration,
    ) -> Result<PathBuf> {
        let job = self.status(id, wait).await?;
        match &job.status {
            JobStatus::Completed => {}
            JobStatus::Failed { message } => {
                bail!("job failed: {message}; call `retry` to resubmit it")
            }
            _ => bail!(
                "job is still {} after waiting {}s; call `download` again to keep waiting",
                if matches!(job.status, JobStatus::Queued) {
                    "queued"
                } else {
                    "running"
                },
                wait.as_secs()
            ),
        }
        let remote = {
            let j = self.jobs.lock().await;
            j.get(&id)
                .and_then(|e| e.remote_id.clone())
                .context("missing remote id")?
        };
        let out = out.unwrap_or_else(|| default_output(&job));
        let out = if out.is_absolute() {
            out
        } else {
            std::env::current_dir()?.join(out)
        };
        self.backend.lock().await.download(&remote, &out).await?;
        Ok(out)
    }

    pub async fn retry(&self, id: Uuid) -> Result<Job> {
        let request = {
            let j = self.jobs.lock().await;
            j.get(&id).context("unknown job")?.request.clone()
        };
        self.submit(request).await
    }

    fn spawn(&self, id: Uuid, _retry: bool) {
        let backend = self.backend.clone();
        let slots = self.slots.clone();
        let jobs = self.jobs.clone();
        tokio::spawn(async move {
            let Ok(permit) = slots.acquire_owned().await else {
                return;
            };
            let request = {
                let j = jobs.lock().await;
                match j.get(&id) {
                    Some(e) => e.request.clone(),
                    None => return,
                }
            };
            let remote = match backend.lock().await.submit(&request).await {
                Ok(v) => v,
                Err(e) => {
                    if let Some(x) = jobs.lock().await.get_mut(&id) {
                        x.status = JobStatus::Failed {
                            message: e.to_string(),
                        };
                    }
                    return;
                }
            };
            if let Some(x) = jobs.lock().await.get_mut(&id) {
                x.remote_id = Some(remote.clone());
                x.status = JobStatus::Running;
            }
            let deadline = tokio::time::Instant::now() + Duration::from_secs(25 * 60);
            loop {
                tokio::time::sleep(Duration::from_secs(3)).await;
                if tokio::time::Instant::now() >= deadline {
                    if let Some(x) = jobs.lock().await.get_mut(&id) {
                        x.status = JobStatus::Failed {
                            message: "status timeout after 25 minutes".into(),
                        };
                    }
                    break;
                }
                let status = backend.lock().await.status(&remote).await;
                if let Ok(s) = status {
                    let terminal = s.is_terminal();
                    if let Some(x) = jobs.lock().await.get_mut(&id) {
                        x.status = s;
                    }
                    if terminal {
                        break;
                    }
                }
            }
            drop(permit);
        });
    }
}
/// `downloads/<name|prompt|image stem>-<id prefix>.glb`
fn default_output(job: &Job) -> PathBuf {
    let r = &job.request;
    let stem = r
        .image
        .as_ref()
        .and_then(|p| p.file_stem())
        .and_then(|s| s.to_str());
    let label = r
        .name
        .as_deref()
        .or(Some(r.prompt.as_str()).filter(|p| !p.trim().is_empty()))
        .or(stem)
        .unwrap_or("model");
    let mut slug = String::new();
    for c in label.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
        if slug.chars().count() >= 40 {
            break;
        }
    }
    let slug = slug.trim_matches('-');
    let short = &job.id.simple().to_string()[..8];
    PathBuf::from("downloads").join(format!(
        "{}-{short}.glb",
        if slug.is_empty() { "model" } else { slug }
    ))
}

fn validate(r: &GenerateRequest) -> Result<()> {
    if !POLYCOUNTS.contains(&r.polycount) {
        bail!(
            "unsupported polycount {}; use one of {POLYCOUNTS:?}",
            r.polycount
        );
    }
    if r.image.is_none() && r.prompt.trim().is_empty() {
        bail!("provide `prompt` (Text-to-3D) or `image` (Image-to-3D)");
    }
    if let Some(p) = &r.image
        && !p.is_file()
    {
        bail!("image not found: {}", p.display());
    }
    Ok(())
}
