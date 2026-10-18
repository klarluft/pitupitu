//! `pitupitu`, the box agent: it links this machine to pitupitu cloud,
//! where the conversation happens. `setup` signs the box in, makes the home
//! folder and installs the user service; `connect` is what the service
//! runs; `prefs`, `box`, `default` and `settings` change the account's
//! settings; `--skill` teaches the person's agents all of this.

mod calls;
mod config;
mod device;
mod home;
mod inventory;
mod link;
mod router;
mod service;
mod settings;
mod skill;
mod term;
#[cfg(test)]
mod testhost;

use anyhow::{bail, Result};
use clap::{CommandFactory, Parser, Subcommand};
use config::Config;
use herdr::HerdrCli;
use serde_json::{json, Value};
use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser)]
#[command(
    name = "pitupitu",
    version,
    about = "Links this machine's coding agents (in herdr, or in its own terminals) to pitupitu cloud"
)]
struct Cli {
    /// Config file (default: $PITUPITU_CONFIG or ~/.config/pitupitu/pitupitu.toml).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Print JSON instead of text (settings commands).
    #[arg(long, global = true)]
    json: bool,
    /// Print the skill that teaches an agent to install and configure pitupitu.
    #[arg(long)]
    skill: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
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
    /// What pitupitu knows about you and your machines (read in every conversation).
    Prefs {
        #[command(subcommand)]
        action: Option<PrefsCmd>,
    },
    /// This box's note, driver and on/off.
    #[command(name = "box")]
    Box {
        #[command(subcommand)]
        action: BoxCmd,
    },
    /// What pitupitu brings up between turns unasked: mine (only what it
    /// started), all (your own sessions too) or quiet (nothing unless asked).
    Updates {
        #[arg(value_parser = clap::builder::PossibleValuesParser::new(settings::UPDATES))]
        mode: String,
    },
    /// The agent a session gets when you don't say: claude, codex, or auto
    /// (whatever the machine has, Claude first).
    Agent {
        #[arg(value_parser = clap::builder::PossibleValuesParser::new(settings::AGENTS))]
        name: String,
    },
    /// The model an agent runs on when you don't say (`pitupitu model claude opus`).
    Model {
        /// claude or codex.
        #[arg(value_parser = clap::builder::PossibleValuesParser::new(settings::AGENT_KINDS))]
        agent: String,
        /// The model as the CLI names it (opus, sonnet, gpt-5, …).
        #[arg(required_unless_present = "clear")]
        model: Option<String>,
        /// Back to the agent's own default model.
        #[arg(long)]
        clear: bool,
    },
    /// The box used when a request does not say which.
    Default {
        /// A box name, as `pitupitu settings` lists them.
        #[arg(required_unless_present = "clear")]
        name: Option<String>,
        /// No default: pitupitu asks.
        #[arg(long)]
        clear: bool,
    },
    /// All of the account's settings and boxes.
    Settings,
    /// The pitupitu skill for Claude Code and Codex.
    Skill {
        #[command(subcommand)]
        action: SkillCmd,
    },
    /// Show a session of this box's own terminals live here (Ctrl-\\ detaches).
    Attach {
        /// The session's name or pane, as `pitupitu sessions` lists them
        /// (default: the only one, or the newest).
        session: Option<String>,
    },
    /// The sessions in this box's own terminals.
    Sessions,
    /// Hold the box's own terminals (started by `connect` when needed).
    #[command(hide = true)]
    Terminals,
    /// What an agent's hook reports (run by the agents themselves).
    #[command(hide = true)]
    Hook {
        pane: String,
        event: String,
        arg: Option<String>,
    },
}

#[derive(Subcommand)]
enum PrefsCmd {
    /// Print the preferences (the default).
    Show,
    /// Edit them in $VISUAL or $EDITOR.
    Edit,
    /// Replace them with a file's text (`-` reads stdin).
    Set { file: PathBuf },
    /// Remove them.
    Clear,
}

#[derive(Subcommand)]
enum BoxCmd {
    /// One line about this box ("client work, Acme").
    Note {
        #[arg(required_unless_present = "clear")]
        text: Vec<String>,
        #[arg(long)]
        clear: bool,
    },
    /// What runs agent sessions here: auto (herdr when it runs), herdr or terminal.
    Driver {
        #[arg(value_parser = clap::builder::PossibleValuesParser::new(settings::DRIVERS))]
        driver: String,
    },
    /// Switch this box on: pitupitu uses it again.
    On,
    /// Switch this box off: it stays linked, but pitupitu starts nothing here
    /// and does not watch its sessions until it is on again.
    Off,
    /// This box's own default agent: claude, codex, or auto (the account's).
    Agent {
        #[arg(value_parser = clap::builder::PossibleValuesParser::new(settings::AGENTS))]
        name: String,
    },
}

#[derive(Subcommand)]
enum SkillCmd {
    /// Print SKILL.md (same as `pitupitu --skill`).
    Print,
    /// Install it for Claude Code (~/.claude/skills) and Codex (~/.codex/skills).
    Install,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("PITUPITU_LOG")
                .unwrap_or_else(|_| "pitupitu=info,herdr=warn".into()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();
    let cli = Cli::parse();
    let path = cli.config.unwrap_or_else(config::default_path);
    let as_json = cli.json;
    if cli.skill {
        print!("{}", skill::SKILL);
        return Ok(());
    }
    let Some(cmd) = cli.cmd else {
        Cli::command().print_help()?;
        return Ok(());
    };
    match cmd {
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
        Cmd::Prefs { action } => {
            let cfg = Config::load(&path)?;
            let changed = match action.unwrap_or(PrefsCmd::Show) {
                PrefsCmd::Show => {
                    let s = settings::get(&cfg).await?;
                    if as_json {
                        println!("{}", json!({"preferences": s["preferences"]}));
                    } else {
                        match s["preferences"].as_str() {
                            Some(p) => println!("{p}"),
                            None => {
                                eprintln!("no preferences yet; `pitupitu prefs edit` writes some")
                            }
                        }
                    }
                    return Ok(());
                }
                PrefsCmd::Edit => match settings::edit(&cfg).await? {
                    Some(s) => s,
                    None => {
                        eprintln!("unchanged");
                        return Ok(());
                    }
                },
                PrefsCmd::Set { file } => {
                    let text = settings::clean(&settings::read_text(&file)?);
                    settings::patch(&cfg, json!({"preferences": settings::text_or_null(&text)}))
                        .await?
                }
                PrefsCmd::Clear => settings::patch(&cfg, json!({"preferences": null})).await?,
            };
            done(&changed, as_json, "preferences saved");
            Ok(())
        }
        Cmd::Box { action } => {
            let cfg = Config::load(&path)?;
            let change = match action {
                BoxCmd::Note { text, clear } => {
                    let note = if clear { String::new() } else { text.join(" ") };
                    json!({"note": settings::text_or_null(&note)})
                }
                BoxCmd::Driver { driver } => json!({"driver": driver}),
                BoxCmd::On => json!({"enabled": true}),
                BoxCmd::Off => json!({"enabled": false}),
                BoxCmd::Agent { name } => json!({"agent": settings::agent_or_null(&name)}),
            };
            let s = settings::patch(&cfg, change).await?;
            done(&s, as_json, "saved");
            Ok(())
        }
        Cmd::Default { name, clear } => {
            let cfg = Config::load(&path)?;
            let value = if clear { Value::Null } else { json!(name) };
            let s = settings::patch(&cfg, json!({"default_box": value})).await?;
            done(&s, as_json, "saved");
            Ok(())
        }
        Cmd::Updates { mode } => {
            let cfg = Config::load(&path)?;
            let s = settings::patch(&cfg, json!({"updates": mode})).await?;
            done(&s, as_json, "saved");
            Ok(())
        }
        Cmd::Agent { name } => {
            let cfg = Config::load(&path)?;
            let s = settings::patch(
                &cfg,
                json!({"default_agent": settings::agent_or_null(&name)}),
            )
            .await?;
            done(&s, as_json, "saved");
            Ok(())
        }
        Cmd::Model {
            agent,
            model,
            clear,
        } => {
            let cfg = Config::load(&path)?;
            let value = if clear {
                Value::Null
            } else {
                json!(model.unwrap_or_default().trim().to_lowercase())
            };
            let s = settings::patch(&cfg, json!({"agent_models": {agent: value}})).await?;
            done(&s, as_json, "saved");
            Ok(())
        }
        Cmd::Settings => {
            let cfg = Config::load(&path)?;
            settings::print(&settings::get(&cfg).await?, as_json);
            Ok(())
        }
        Cmd::Terminals => term::holder::run(term::socket_path()).await,
        Cmd::Hook { pane, event, arg } => {
            term::client::hook(&pane, &event, arg.as_deref()).await;
            Ok(())
        }
        Cmd::Sessions => sessions(as_json).await,
        Cmd::Attach { session } => {
            let target = match session {
                Some(s) => s,
                None => {
                    let list = term_agents().await;
                    match list.last() {
                        Some(a) => a.target().to_string(),
                        None => bail!("no sessions in this box's own terminals"),
                    }
                }
            };
            term::attach::attach(&target).await
        }
        Cmd::Skill { action } => {
            match action {
                SkillCmd::Print => print!("{}", skill::SKILL),
                SkillCmd::Install => {
                    for f in skill::install(&config::home_dir())? {
                        eprintln!("wrote {}", f.display());
                    }
                }
            }
            Ok(())
        }
    }
}

async fn term_agents() -> Vec<herdr::AgentInfo> {
    use herdr::AgentHost;
    term::client::TermHost::default()
        .list_agents()
        .await
        .unwrap_or_default()
}

/// `pitupitu sessions`: the terminal host's agents.
async fn sessions(as_json: bool) -> Result<()> {
    let list = term_agents().await;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&list)?);
        return Ok(());
    }
    if list.is_empty() {
        println!("no sessions in this box's own terminals");
    }
    for a in list {
        println!(
            "{}  {} {} in {}{}",
            a.target(),
            a.agent,
            a.agent_status,
            a.cwd.display(),
            a.terminal_title_stripped
                .as_deref()
                .map(|t| format!(" — {t}"))
                .unwrap_or_default()
        );
    }
    Ok(())
}

/// After a change: the settings as they now are.
fn done(settings: &Value, as_json: bool, what: &str) {
    if as_json {
        settings::print(settings, true);
    } else {
        eprintln!("{what}");
        settings::print(settings, false);
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
    let available = inventory::available().await;
    let router = Arc::new(router::Router::new(
        Arc::new(herdr),
        Arc::new(term::client::TermHost::default()),
    ));
    let link = link::Link {
        url: cfg.box_url(),
        api_key,
        box_name: cfg.box_name(),
        home,
        herdr_version,
        host: router.clone(),
        available,
        history: Some(config::home_dir()),
        router: Some(router),
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

    // 3. The skill, offered once to a person at a terminal.
    let skill_file = config::home_dir().join(".claude/skills/pitupitu/SKILL.md");
    if !skill_file.exists() {
        if std::io::stdin().is_terminal() {
            let yes = ask(
                "teach Claude Code and Codex how to configure pitupitu (installs a skill)? y/n",
                "y",
            );
            if yes.to_lowercase().starts_with('y') {
                for f in skill::install(&config::home_dir())? {
                    eprintln!("  wrote {}", f.display());
                }
            }
        } else {
            eprintln!(
                "  `pitupitu skill install` teaches Claude Code and Codex to configure pitupitu"
            );
        }
    }

    // 4. The service.
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
    let agents = inventory::available().await;
    println!(
        "agents: {}",
        if agents.is_empty() {
            "none found on PATH".to_string()
        } else {
            agents
                .iter()
                .map(|a| format!("{} ({})", a.kind, a.version))
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
    let recent = inventory::recent(&config::home_dir());
    println!("recent project folders: {}", recent.len());
    let own = term_agents().await;
    println!(
        "own terminals: {} session{} (`pitupitu sessions`)",
        own.len(),
        if own.len() == 1 { "" } else { "s" }
    );
    println!("service: {}", service::status());
    Ok(())
}
