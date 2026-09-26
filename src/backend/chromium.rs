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
