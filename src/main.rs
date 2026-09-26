use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use rmcp::ServiceExt;
use sloyd_mcp::{
    api::AgentApi,
    backend::{Backend, sloyd_cdp::SloydWebBackend},
    cookies::import_cookie_file,
    mcp::SloydMcp,
    service::Service,
};
use std::{path::PathBuf, sync::Arc};

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Engine {
    Auto,
    Lightpanda,
    Chromium,
}
#[derive(Parser, Debug)]
struct Cli {
    #[arg(long,value_enum,default_value_t=Engine::Auto)]
    engine: Engine,
    #[arg(long, default_value = "/root/sloyd-mcp/bin/lightpanda")]
    lightpanda: PathBuf,
    #[arg(long, default_value = "/usr/bin/chromium")]
    chromium: PathBuf,
    #[arg(long, default_value = "/root/sloyd-mcp/state/chromium")]
    profile: PathBuf,
    #[arg(long, default_value = "/root/sloyd-mcp/state/cookies.json")]
    cookies: PathBuf,
    #[arg(long, default_value = "/root/sloyd-mcp/state/cache")]
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
async fn backend(cli: &Cli) -> Result<SloydWebBackend> {
    match cli.engine {
        Engine::Lightpanda => light(cli).await,
        Engine::Chromium => chrome(cli).await,
        Engine::Auto => match light(cli).await {
            Ok(b) => Ok(b),
            Err(_) => chrome(cli).await,
        },
    }
}
#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match &cli.command {
        Command::Doctor => {
            println!("lightpanda={}", cli.lightpanda.is_file());
            println!("chromium={}", cli.chromium.is_file());
            println!("cookies={}", cli.cookies.display());
        }
        Command::SessionProbe => {
            let b = backend(&cli).await?;
            println!("authenticated={}", b.session_probe().await?);
        }
        Command::ImportCookies { source } => {
            let imported = import_cookie_file(source).await?;
            if let Some(p) = cli.cookies.parent() {
                tokio::fs::create_dir_all(p).await?;
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
        Command::Mcp { concurrency } => {
            let b = backend(&cli).await?;
            b.health().await?;
            let api: Arc<dyn AgentApi> = Arc::new(Service::new(b, *concurrency)?);
            SloydMcp::new(api)
                .serve(rmcp::transport::stdio())
                .await?
                .waiting()
                .await?;
        }
    }
    Ok(())
}
