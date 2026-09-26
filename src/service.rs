use crate::{
    backend::Backend,
    types::{GenerateRequest, Job, JobStatus},
};
use anyhow::{Context, Result, bail};
use std::{collections::HashMap, path::Path, sync::Arc, time::Duration};
use tokio::sync::{Mutex, Semaphore};
use uuid::Uuid;

struct Entry {
    request: GenerateRequest,
    remote_id: Option<String>,
    status: JobStatus,
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
            },
        );
        self.spawn(id, false);
        Ok(Job {
            id,
            status: JobStatus::Queued,
        })
    }

    pub async fn status(&self, id: Uuid) -> Result<Job> {
        let j = self.jobs.lock().await;
        let e = j.get(&id).context("unknown job")?;
        Ok(Job {
            id,
            status: e.status.clone(),
        })
    }

    pub async fn download(&self, id: Uuid, out: &Path) -> Result<()> {
        let remote = {
            let j = self.jobs.lock().await;
            let e = j.get(&id).context("unknown job")?;
            if !matches!(e.status, JobStatus::Completed) {
                bail!("job is not completed");
            }
            e.remote_id.clone().context("missing remote id")?
        };
        self.backend.lock().await.download(&remote, out).await
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
                    let terminal = matches!(s, JobStatus::Completed | JobStatus::Failed { .. });
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
fn validate(r: &GenerateRequest) -> Result<()> {
    const OK: &[u32] = &[3000, 4000, 5000, 10000, 20000, 40000, 100000];
    if !OK.contains(&r.polycount) {
        bail!("unsupported polycount");
    }
    if r.image.is_none() && r.prompt.trim().is_empty() {
        bail!("text-to-3d requires prompt");
    }
    if let Some(p) = &r.image
        && !p.is_file()
    {
        bail!("image not found: {}", p.display());
    }
    Ok(())
}
