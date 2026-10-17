//! `pitupitu`, the box agent: it links herdr on this machine to pitupitu
//! cloud, where the conversation happens. `setup` signs the box in, makes
//! the home folder and installs the user service; `connect` is what the
//! service runs.

mod calls;
mod config;
mod device;
mod home;
mod link;
mod service;
#[cfg(test)]
mod testhost;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use config::Config;
use herdr::HerdrCli;
use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser)]
#[command(
    name = "pitupitu",
    version,
    about = "Links herdr on this machine to pitupitu cloud"
)]
struct Cli {
    /// Config file (default: $PITUPITU_CONFIG or ~/.config/pitupitu/pitupitu.toml).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Sign this box in, create the home folder, install the user service.
    Setup {
        /// The home folder (default: ~/pitupitu, or the one set before).
        #[arg(long)]
        home: Option<String>,
        /// Sign in again even if this box already has a working key.
        #[arg(long)]
        relink: bool,
        /// How this box is named on the dashboard (default: hostname).
        #[arg(long = "box")]
        box_name: Option<String>,
        /// Do not install or restart the user service.
        #[arg(long)]
        no_service: bool,
        /// Only print the sign-in URL; do not try to open a browser.
        #[arg(long)]
        no_browser: bool,
    },
    /// Hold the link to pitupitu cloud open (what the service runs).
    Connect,
    /// The account, the home folder and the service, as this box sees them.
    Status,
    /// Stop and remove the user service. The config and home folder stay.
    Uninstall,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("PITUPITU_LOG")
                .unwrap_or_else(|_| "pitupitu=info,herdr=warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let path = cli.config.unwrap_or_else(config::default_path);
    match cli.cmd {
        Cmd::Setup {
            home,
            relink,
            box_name,
            no_service,
            no_browser,
        } => {
            setup(
                &path,
                home.as_deref(),
                relink,
                box_name,
                no_service,
                no_browser,
            )
            .await
        }
        Cmd::Connect => connect(&Config::load(&path)?).await,
        Cmd::Status => status(&Config::load(&path)?).await,
        Cmd::Uninstall => {
            match service::uninstall()? {
                Some(file) => eprintln!("removed {}", file.display()),
                None => eprintln!("no pitupitu service was installed"),
            }
            Ok(())
        }
    }
}

async fn connect(cfg: &Config) -> Result<()> {
    let Some(api_key) = cfg.api_key.clone() else {
        bail!("this box is not signed in; run `pitupitu setup`");
    };
    let home = cfg.home.clone().unwrap_or_else(config::default_home);
    let herdr = HerdrCli::new();
    let herdr_version = herdr.version().await.unwrap_or_else(|e| {
        tracing::warn!("herdr: {e}");
        String::new()
    });
    let link = link::Link {
        url: cfg.box_url(),
        api_key,
        box_name: cfg.box_name(),
        home,
        herdr_version,
        host: Arc::new(herdr),
        ping_timeout: link::PING_TIMEOUT,
    };
    tokio::select! {
        r = link.run() => r,
        _ = shutdown() => Ok(()),
    }
}

async fn shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("signal handler");
        tokio::select! {
            _ = term.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

fn ask(question: &str, default: &str) -> String {
    if !std::io::stdin().is_terminal() {
        return default.to_string();
    }
    eprint!("  {question} [{default}]: ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    let line = line.trim();
    if line.is_empty() {
        default.to_string()
    } else {
        line.to_string()
    }
}

async fn setup(
    path: &std::path::Path,
    home: Option<&str>,
    relink: bool,
    box_name: Option<String>,
    no_service: bool,
    no_browser: bool,
) -> Result<()> {
    let cfg = Config::load(path)?;

    // 1. The key.
    let mut linked = None;
    if let (Some(key), false) = (&cfg.api_key, relink) {
        match device::me(&cfg.cloud_url, key).await {
            Ok(me) => linked = Some(me),
            Err(device::MeError::Rejected) => {
                eprintln!("  this box's key no longer works; signing in again")
            }
            Err(e) => bail!("{e}"),
        }
    }
    match linked {
        Some(me) => eprintln!(
            "  already signed in as {} (box \"{}\")",
            me.email, me.box_name
        ),
        None => {
            let name = box_name.unwrap_or_else(|| cfg.box_name());
            let issued = device::login(&cfg.cloud_url, &name, !no_browser).await?;
            let mut entries = vec![
                ("cloud", "api_key", issued.api_key.as_str()),
                ("cloud", "box", issued.box_name.as_str()),
            ];
            if cfg.cloud_url != config::DEFAULT_CLOUD_URL {
                entries.push(("cloud", "url", cfg.cloud_url.as_str()));
            }
            config::set(path, &entries)?;
            eprintln!();
            eprintln!(
                "  signed in as {}; this box is \"{}\" on the dashboard",
                issued.email, issued.box_name
            );
        }
    }

    // 2. The home folder.
    let current = cfg.home.clone().unwrap_or_else(config::default_home);
    let home = match home {
        Some(h) => config::expand(h),
        None => config::expand(&ask(
            "home folder for pitupitu",
            &current.display().to_string(),
        )),
    };
    for created in home::ensure(&home)? {
        eprintln!("  created {created}");
    }
    config::set(path, &[("home", "path", &home.display().to_string())])?;
    eprintln!("  home folder: {}", home.display());

    // 3. The service.
    if no_service {
        eprintln!();
        eprintln!("  run `pitupitu connect` to link this box");
    } else {
        let installed = service::install(path)?;
        eprintln!(
            "  service installed and running: {}",
            installed.file.display()
        );
        eprintln!("  logs: {}", installed.logs);
        if let Some(hint) = service::linger_hint() {
            eprintln!("  {hint}");
        }
    }
    eprintln!();
    eprintln!("  now open https://pitupitu.chat/app");
    Ok(())
}

async fn status(cfg: &Config) -> Result<()> {
    println!("config: {}", cfg.path.display());
    println!("cloud: {}", cfg.cloud_url);
    match &cfg.api_key {
        None => println!("account: not signed in (run `pitupitu setup`)"),
        Some(key) => match device::me(&cfg.cloud_url, key).await {
            Ok(me) => println!("{}", me.describe()),
            Err(e) => println!("account: {e}"),
        },
    }
    let home = cfg.home.clone().unwrap_or_else(config::default_home);
    let files = home::scan(&home);
    println!(
        "home: {} ({} pitupitu file{})",
        home.display(),
        files.len(),
        if files.len() == 1 { "" } else { "s" }
    );
    let herdr = HerdrCli::new()
        .version()
        .await
        .unwrap_or_else(|e| format!("not found ({e})"));
    println!("herdr: {herdr}");
    println!("service: {}", service::status());
    Ok(())
}
