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
