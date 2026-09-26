use anyhow::{Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use rmcp::ServiceExt;
use sloyd_mcp::{
    api::AgentApi,
    backend::{Backend, sloyd_cdp::SloydWebBackend, sloyd_http::SloydHttpBackend},
    cookies::import_cookie_file,
    mcp::SloydMcp,
    service::Service,
};
use std::{env, path::PathBuf, sync::Arc};

fn config_dir() -> PathBuf {
    if let Some(path) = env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(path).join("sloyd-mcp");
    }
    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home).join(".config/sloyd-mcp");
    }
    PathBuf::from(".config/sloyd-mcp")
}

fn default_cookie_path() -> PathBuf {
    config_dir().join("cookies.json")
}

fn default_profile_path() -> PathBuf {
    config_dir().join("chromium")
}

fn default_cache_path() -> PathBuf {
    config_dir().join("cache")
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Engine {
    Auto,
    Http,
    Lightpanda,
    Chromium,
}

#[derive(Parser, Debug)]
struct Cli {
    #[arg(long, value_enum, default_value_t = Engine::Auto)]
    engine: Engine,

    #[arg(long, default_value = "/root/sloyd-mcp/bin/lightpanda")]
    lightpanda: PathBuf,

    #[arg(long, default_value = "/usr/bin/chromium")]
    chromium: PathBuf,

    #[arg(long, default_value_os_t = default_profile_path())]
    profile: PathBuf,

    #[arg(long, default_value_os_t = default_cookie_path())]
    cookies: PathBuf,

    #[arg(long, default_value_os_t = default_cache_path())]
    cache: PathBuf,

    #[arg(long, default_value_t = 9232)]
    port: u16,

    #[arg(long, default_value_t = false)]
    allow_guest: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Doctor,
    SessionProbe,
    ImportCookies {
        source: PathBuf,
    },
    Mcp {
        #[arg(long, default_value_t = 2)]
        concurrency: usize,
    },
}

async fn light(cli: &Cli) -> Result<SloydWebBackend> {
    SloydWebBackend::launch_lightpanda(
        &cli.lightpanda,
        &cli.cookies,
        &cli.cache,
        cli.port,
        cli.allow_guest,
    )
    .await
}

async fn chrome(cli: &Cli) -> Result<SloydWebBackend> {
    SloydWebBackend::launch_chromium(
        &cli.chromium,
        &cli.profile,
        &cli.cookies,
        cli.port,
        cli.allow_guest,
    )
    .await
}

async fn http(cli: &Cli) -> Result<SloydHttpBackend> {
    SloydHttpBackend::new(&cli.cookies)
}

async fn run_mcp<B>(backend: B, concurrency: usize) -> Result<()>
where
    B: Backend + Send + 'static,
{
    let api: Arc<dyn AgentApi> = Arc::new(Service::new(backend, concurrency)?);
    SloydMcp::new(api)
        .serve(rmcp::transport::stdio())
        .await?
        .waiting()
        .await?;
    Ok(())
}

async fn probe_auto(cli: &Cli) -> Result<bool> {
    let mut backend = http(cli).await?;
    match backend.session_probe().await {
        Ok(true) => Ok(true),
        Ok(false) | Err(_) => {
            if cli.lightpanda.is_file() {
                return light(cli).await?.session_probe().await;
            }
            if cli.chromium.is_file() {
                return chrome(cli).await?.session_probe().await;
            }
            Ok(false)
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match &cli.command {
        Command::Doctor => {
            println!("cookies={}", cli.cookies.display());
            println!("browserless_http=true");
            println!("lightpanda_fallback={}", cli.lightpanda.is_file());
            println!("chromium_fallback={}", cli.chromium.is_file());
        }
        Command::SessionProbe => {
            let authenticated = match cli.engine {
                Engine::Auto => probe_auto(&cli).await?,
                Engine::Http => http(&cli).await?.session_probe().await?,
                Engine::Lightpanda => light(&cli).await?.session_probe().await?,
                Engine::Chromium => chrome(&cli).await?.session_probe().await?,
            };
            println!("authenticated={authenticated}");
        }
        Command::ImportCookies { source } => {
            let imported = import_cookie_file(source).await?;
            if let Some(parent) = cli.cookies.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&cli.cookies, serde_json::to_vec_pretty(&imported.cookies)?).await?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                tokio::fs::set_permissions(&cli.cookies, std::fs::Permissions::from_mode(0o600))
                    .await?;
            }
            println!("imported_sloyd_cookies={}", imported.cookies.len());
            println!("dropped_non_sloyd={}", imported.dropped_non_sloyd);
        }
        Command::Mcp { concurrency } => match cli.engine {
            Engine::Http => {
                let mut backend = http(&cli).await?;
                if !backend.session_probe().await? {
                    bail!("browserless Auth0 session is not authenticated");
                }
                run_mcp(backend, *concurrency).await?;
            }
            Engine::Lightpanda => run_mcp(light(&cli).await?, *concurrency).await?,
            Engine::Chromium => run_mcp(chrome(&cli).await?, *concurrency).await?,
            Engine::Auto => {
                let mut backend = http(&cli).await?;
                if backend.session_probe().await.unwrap_or(false) {
                    run_mcp(backend, *concurrency).await?;
                } else if cli.lightpanda.is_file() {
                    run_mcp(light(&cli).await?, *concurrency).await?;
                } else if cli.chromium.is_file() {
                    run_mcp(chrome(&cli).await?, *concurrency).await?;
                } else {
                    bail!(
                        "browserless Auth0 session is unavailable and no browser fallback exists"
                    );
                }
            }
        },
    }

    Ok(())
}
