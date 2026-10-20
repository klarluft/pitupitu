//! `pitupitu`, the box agent: it links this machine's agents, running in
//! herdr, to pitupitu cloud, where the conversation happens. `setup` signs
//! the box in, makes the home folder, installs herdr when it is missing and
//! installs the user service; `connect` is what the service runs; `update`
//! updates the box agent or herdr when the person asks; `prefs`, `box`,
//! `default` and `settings` change the account's settings; `--skill`
//! teaches the person's agents all of this.

use anyhow::{bail, Result};
use clap::{CommandFactory, Parser, Subcommand};
use herdr::HerdrCli;
use pitupitu::config::Config;
use pitupitu::{
    config, device, herdr_box, home, inventory, link, service, settings, skill, update,
};
use serde_json::{json, Value};
use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser)]
#[command(
    name = "pitupitu",
    version,
    about = "Links this machine's coding agents, running in herdr, to pitupitu cloud"
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
    /// Sign this box in, create the home folder, install herdr when it is
    /// missing, install the user service.
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
        /// Linux: keep the service (and herdr) running when nobody is
        /// logged in (`loginctl enable-linger`), without asking.
        #[arg(long, conflicts_with = "no_linger")]
        linger: bool,
        /// Linux: do not offer lingering.
        #[arg(long)]
        no_linger: bool,
    },
    /// Hold the link to pitupitu cloud open (what the service runs).
    Connect,
    /// The account, the home folder, herdr and the service, as this box sees them.
    Status,
    /// Stop and remove the user service. The config, home folder and herdr stay.
    Uninstall,
    /// Update this box agent (the default), herdr, or both: shows the
    /// newer version and what is running, asks, then installs it the way
    /// it was installed. Running sessions are not touched.
    Update {
        /// box (this box agent), herdr, or all.
        #[arg(default_value = "box", value_parser = clap::builder::PossibleValuesParser::new(["box", "herdr", "all"]))]
        what: String,
        /// herdr: restart its server on the new version, keeping the
        /// sessions (best effort). Without it the server keeps running as it is.
        #[arg(long)]
        handoff: bool,
        /// Update without asking (needed when there is no terminal).
        #[arg(long)]
        yes: bool,
        /// Only say whether a newer version exists.
        #[arg(long)]
        check: bool,
        /// Replace the binary but leave the service running the old one.
        #[arg(long)]
        no_restart: bool,
    },
    /// What pitupitu knows about you and your machines (read in every conversation).
    Prefs {
        #[command(subcommand)]
        action: Option<PrefsCmd>,
    },
    /// This box's name, note, agent and on/off.
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
    /// Folders on this box with a name you say ("in acme, run the tests").
    Place {
        #[command(subcommand)]
        action: PlaceCmd,
    },
    /// Every named folder, on every box.
    Places,
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
    /// Rename this box: what you call it when you talk ("on the mac, …").
    Rename {
        #[arg(required = true)]
        name: Vec<String>,
    },
}

#[derive(Subcommand)]
enum PlaceCmd {
    /// Name a folder on this box (`pitupitu place add acme ~/work/acme-api`).
    Add {
        /// One or two words, as you will say them.
        name: String,
        /// The folder (made absolute).
        path: PathBuf,
    },
    /// Drop a name; the folder is untouched.
    Rm { name: String },
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
            linger,
            no_linger,
        } => {
            let linger = match (linger, no_linger) {
                (true, _) => Some(true),
                (_, true) => Some(false),
                _ => None,
            };
            setup(
                &path,
                home.as_deref(),
                relink,
                box_name,
                no_service,
                no_browser,
                linger,
            )
            .await
        }
        Cmd::Connect => connect(&Config::load(&path)?).await,
        Cmd::Status => status(&Config::load(&path)?).await,
        Cmd::Update {
            what,
            handoff,
            yes,
            check,
            no_restart,
        } => {
            update_cmd(UpdateArgs {
                what,
                handoff,
                yes,
                check,
                no_restart,
                as_json,
            })
            .await
        }
        Cmd::Uninstall => {
            match service::uninstall()? {
                Some(file) => eprintln!("removed {}", file.display()),
                None => eprintln!("no pitupitu service was installed"),
            }
            eprintln!(
                "herdr is left as it is, with its server and sessions; remove it the way it was \
                 installed if you want it gone"
            );
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
            if let BoxCmd::Rename { name } = &action {
                let s = settings::patch(&cfg, json!({"name": name.join(" ")})).await?;
                // The name `setup --relink` would ask for, kept in step.
                if let Some(now) = s["box"].as_str() {
                    config::set(&path, &[("cloud", "box", now)])?;
                }
                done(
                    &s,
                    as_json,
                    "renamed; the link comes back under the new name in a moment",
                );
                return Ok(());
            }
            let change = match action {
                BoxCmd::Note { text, clear } => {
                    let note = if clear { String::new() } else { text.join(" ") };
                    json!({"note": settings::text_or_null(&note)})
                }
                BoxCmd::On => json!({"enabled": true}),
                BoxCmd::Off => json!({"enabled": false}),
                BoxCmd::Agent { name } => json!({"agent": settings::agent_or_null(&name)}),
                BoxCmd::Rename { .. } => unreachable!("handled above"),
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
        Cmd::Place { action } => {
            let cfg = Config::load(&path)?;
            let change = match action {
                PlaceCmd::Add { name, path } => {
                    let abs = std::path::absolute(&path)
                        .map_err(|e| anyhow::anyhow!("resolving {}: {e}", path.display()))?;
                    let name = name.trim().to_lowercase();
                    json!({"places": {name: abs.to_string_lossy()}})
                }
                PlaceCmd::Rm { name } => json!({"places": {name.trim().to_lowercase(): null}}),
            };
            let s = settings::patch(&cfg, change).await?;
            done(&s, as_json, "saved");
            Ok(())
        }
        Cmd::Places => {
            let cfg = Config::load(&path)?;
            let s = settings::get(&cfg).await?;
            if as_json {
                println!("{}", json!({"places": s["places"]}));
            } else {
                settings::print_places(&s);
            }
            Ok(())
        }
        Cmd::Settings => {
            let cfg = Config::load(&path)?;
            settings::print(&settings::get(&cfg).await?, as_json);
            Ok(())
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
    match herdr_box::status(&herdr).await {
        Some(s) => tracing::info!(version = %s.version, running = s.running, "herdr"),
        None => tracing::warn!(
            "herdr is not installed: pitupitu runs your agents in herdr; `pitupitu setup` installs it"
        ),
    }
    let available = inventory::available().await;
    // herdr's server is up before the cloud is reached, so the person
    // opening `herdr` at the desk finds the same one.
    let keeper = Arc::new(herdr_box::Keeper::new(herdr.clone()));
    keeper.ensure(&herdr).await;
    let link = link::Link {
        url: cfg.box_url(),
        api_key,
        box_name: cfg.box_name(),
        home,
        host: Arc::new(herdr.clone()),
        keeper: Some(keeper),
        available,
        history: Some(config::home_dir()),
        ping_timeout: link::PING_TIMEOUT,
        updates: true,
        releases: Default::default(),
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
    linger: Option<bool>,
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

    // 4. herdr, which runs the agents.
    setup_herdr().await;

    // 5. The service.
    if !no_service {
        offer_linger(path, linger)?;
    }
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
    }
    // Another cloud (a local gateway) has its own site, which the box does not know.
    if cfg.cloud_url == config::DEFAULT_CLOUD_URL {
        eprintln!();
        eprintln!("  now open https://pitupitu.chat/app");
    }
    Ok(())
}

/// herdr is how pitupitu runs agents: installed with its own installer
/// when missing (said in one line, not asked), then herdr's integrations
/// for the agents found here, so herdr resumes their sessions after its
/// server restarts. Failures are said and setup goes on; the next setup
/// tries again.
async fn setup_herdr() {
    let bin = match herdr_box::find_binary() {
        Some(b) => b,
        None => {
            eprintln!("  pitupitu runs your agents in herdr; installing it");
            match herdr_box::install() {
                Ok(b) => {
                    eprintln!("  herdr installed: {}", b.display());
                    b
                }
                Err(e) => {
                    eprintln!(
                        "  could not install herdr ({e:#}); install it from https://herdr.dev, \
                         then run `pitupitu setup` again"
                    );
                    return;
                }
            }
        }
    };
    let cli = HerdrCli::with_binary(bin.display().to_string());
    for kind in ["claude", "codex"] {
        if herdr_box::on_path(kind).is_none() {
            continue;
        }
        match cli.integration_install(kind).await {
            Ok(_) => eprintln!("  herdr integration for {kind}: installed"),
            Err(e) => eprintln!("  herdr integration for {kind}: {e}"),
        }
    }
    eprintln!("  open `herdr` on this machine to see the sessions your phone sees");
}

/// On Linux, user services stop at logout: offered once, `--linger` or
/// `--no-linger` decide for a script, and without a terminal it is only
/// mentioned.
fn offer_linger(path: &std::path::Path, choice: Option<bool>) -> Result<()> {
    let Some(user) = service::linger_off() else {
        return Ok(());
    };
    let yes = match choice {
        Some(c) => c,
        None if config::get(path, "setup", "linger_offered").is_some() => return Ok(()),
        None if !std::io::stdin().is_terminal() => {
            eprintln!(
                "  to keep pitupitu and herdr running with nobody logged in: loginctl enable-linger {user}"
            );
            return Ok(());
        }
        None => {
            eprintln!(
                "  lingering keeps pitupitu and herdr running when nobody is logged in here, \
                 and starts them after a reboot"
            );
            let answer = ask("turn it on (loginctl enable-linger)? y/n", "n");
            config::set(path, &[("setup", "linger_offered", "yes")])?;
            answer.to_lowercase().starts_with('y')
        }
    };
    if yes {
        match service::enable_linger(&user) {
            Ok(()) => eprintln!("  lingering on for {user}"),
            Err(e) => eprintln!("  could not turn lingering on: {e:#}"),
        }
    }
    Ok(())
}

/// What `pitupitu update` was asked for.
struct UpdateArgs {
    what: String,
    handoff: bool,
    yes: bool,
    check: bool,
    no_restart: bool,
    as_json: bool,
}

/// `pitupitu update [box|herdr|all]`: never silent, never unasked. What
/// would change and what is running are shown first; the person says yes
/// every time (or `--yes` for a script).
async fn update_cmd(a: UpdateArgs) -> Result<()> {
    let mut out = serde_json::Map::new();
    let mut failed = None;
    // herdr first: the box agent's own update ends with its restart.
    if a.what == "herdr" || a.what == "all" {
        match update_herdr(&a).await {
            Ok(v) => {
                out.insert("herdr".into(), v);
            }
            Err(e) => failed = Some(e),
        }
    }
    if a.what == "box" || a.what == "all" {
        match update_box(&a).await {
            Ok(v) => {
                out.insert("box".into(), v);
            }
            Err(e) => failed = failed.or(Some(e)),
        }
    }
    if a.as_json {
        let v = match (a.what.as_str(), out.len()) {
            ("all", _) | (_, 0) => Value::Object(out),
            _ => out.into_iter().next().map(|(_, v)| v).unwrap_or_default(),
        };
        println!("{v}");
    }
    match failed {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Text for a person; nothing when `--json` is printing instead.
macro_rules! say {
    ($a:expr, $($t:tt)*) => {
        if !$a.as_json {
            println!($($t)*);
        }
    };
}

/// The answer to "update now?", or `--yes`. Without a terminal and
/// without `--yes`, nothing is updated.
fn confirmed(a: &UpdateArgs, cmd: &str) -> Result<bool> {
    if a.yes {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        bail!("no terminal to ask on; run `{cmd} --yes` to update without asking");
    }
    let answer = ask("update now? y/n", "n");
    if answer.to_lowercase().starts_with('y') {
        Ok(true)
    } else {
        eprintln!("not updated");
        Ok(false)
    }
}

/// The sessions in herdr right now, one line each, for the question.
async fn running_in_herdr(a: &UpdateArgs) {
    use herdr::AgentHost;
    let agents = HerdrCli::new().list_agents().await.unwrap_or_default();
    say!(
        a,
        "running now: {} session{} in herdr",
        agents.len(),
        if agents.len() == 1 { "" } else { "s" }
    );
    for ag in agents.iter().take(10) {
        say!(
            a,
            "  {} ({}, {}) in {}",
            ag.target(),
            ag.agent,
            ag.agent_status,
            ag.cwd.display()
        );
    }
}

async fn update_box(a: &UpdateArgs) -> Result<Value> {
    let c = update::check_box()
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let newer = c.newer();
    if a.check {
        if newer {
            say!(a, "pitupitu {} -> {}", c.current, c.latest);
            say!(a, "release notes: {}", update::notes_url(&c.latest));
            say!(a, "run `pitupitu update` to install it");
        } else {
            say!(a, "pitupitu {} is the newest release", c.current);
        }
        return Ok(
            json!({"current": c.current, "latest": c.latest, "update_available": newer, "install": c.install}),
        );
    }
    if !newer {
        say!(a, "pitupitu {} is the newest release", c.current);
        return Ok(json!({"what": "box", "current": c.current, "nothing_to_update": true}));
    }
    say!(a, "pitupitu {} -> {}", c.current, c.latest);
    say!(a, "release notes: {}", update::notes_url(&c.latest));
    say!(a, "binary: {}", c.exe.display());
    if c.install == "brew" {
        say!(
            a,
            "installed with Homebrew: updated with `brew upgrade pitupitu`"
        );
    }
    running_in_herdr(a).await;
    say!(
        a,
        "they keep running: herdr is a separate program; only the box agent restarts, and its \
         link to pitupitu cloud drops for a few seconds"
    );
    if !confirmed(a, "pitupitu update")? {
        return Ok(json!({"what": "box", "updated": false}));
    }
    let mut done = update::apply_box(&c)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if c.install == "brew" {
        say!(a, "upgraded pitupitu to {} with Homebrew", done.to);
    } else {
        say!(a, "installed pitupitu {} at {}", done.to, c.exe.display());
    }
    if a.no_restart {
        done.restart = "needed";
        say!(a, "restart `pitupitu connect` (or the service) to run it");
    } else if service::restart()? {
        say!(a, "box agent restarted; sessions untouched");
    } else {
        done.restart = "needed";
        say!(
            a,
            "no service installed; restart `pitupitu connect` yourself"
        );
    }
    Ok(serde_json::to_value(done)?)
}

async fn update_herdr(a: &UpdateArgs) -> Result<Value> {
    let cli = HerdrCli::new();
    let c = herdr_box::check(&cli)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let current = c.status.version.clone();
    let newest = c.newest.clone().unwrap_or_else(|| "?".into());
    let newer = c.newer();
    if a.check {
        if newer {
            say!(a, "herdr {current} -> {newest}");
            say!(a, "run `pitupitu update herdr` to install it");
        } else {
            say!(a, "herdr {current} is the newest release");
        }
        if c.status.restart_needed {
            say!(
                a,
                "herdr's running server is older than the binary and waits for a restart"
            );
        }
        return Ok(
            json!({"current": current, "latest": c.newest, "update_available": newer,
            "install": c.install, "running": c.status.running,
            "restart_needed": c.status.restart_needed}),
        );
    }
    if !newer {
        say!(a, "herdr {current} is the newest release");
        return Ok(json!({"what": "herdr", "current": current, "nothing_to_update": true}));
    }
    say!(a, "herdr {current} -> {newest}");
    say!(
        a,
        "release notes: https://github.com/{}/releases",
        update::HERDR_REPO
    );
    if let Some(b) = &c.status.binary {
        say!(a, "binary: {}", b.display());
    }
    match c.install {
        "brew" => {
            say!(
                a,
                "installed with Homebrew: updated with `brew upgrade herdr`"
            );
            if a.handoff {
                say!(
                    a,
                    "--handoff is for herdr's own updater; Homebrew leaves the server as it is"
                );
            }
        }
        "installer" => say!(a, "updated with herdr's own `herdr update`"),
        _ => bail!(
            "herdr at {} was installed some other way (mise, Nix, a build of your own); \
             update it that way",
            c.status
                .binary
                .as_deref()
                .map_or("an unknown path".into(), |b| b.display().to_string())
        ),
    }
    running_in_herdr(a).await;
    if a.handoff && c.install == "installer" {
        say!(
            a,
            "with --handoff herdr's server restarts on the new version, keeping your sessions \
             (best effort)"
        );
    } else {
        say!(
            a,
            "they keep running: the running server stays as it is and picks up the new version \
             when it next restarts"
        );
    }
    if !confirmed(a, "pitupitu update herdr")? {
        return Ok(json!({"what": "herdr", "updated": false}));
    }
    let done = herdr_box::apply(&cli, &c, a.handoff)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    say!(a, "updated herdr {} -> {}", done.from, done.to);
    match done.restart {
        "needed" => say!(
            a,
            "herdr's running server is still {}: it keeps working and runs {} after it restarts \
             (quit herdr and open it again, or `herdr update --handoff` next time)",
            done.from,
            done.to
        ),
        "done" => say!(a, "herdr's server restarted on {}", done.to),
        _ => {}
    }
    Ok(serde_json::to_value(done)?)
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
    let cli = HerdrCli::new();
    let (herdr, newest_herdr, newer_box) = tokio::join!(
        herdr_box::status(&cli),
        update::herdr_newest(),
        update::newer()
    );
    match herdr {
        None => println!(
            "herdr: not installed; pitupitu runs your agents in herdr (`pitupitu setup` installs it)"
        ),
        Some(s) => {
            let info = herdr_box::HerdrInfo::new(&s, s.running, newest_herdr.as_deref());
            println!(
                "herdr: {}{}, server {}, installed with {}",
                info.version,
                info.protocol
                    .map(|p| format!(" (protocol {p})"))
                    .unwrap_or_default(),
                if info.running {
                    "running"
                } else {
                    "not running (the box agent starts it)"
                },
                match info.install {
                    "brew" => "Homebrew",
                    "installer" => "herdr's installer",
                    _ => "something else",
                }
            );
            if let Some(v) = &info.latest {
                println!("  newer herdr release {v}: `pitupitu update herdr`");
            }
            if info.restart_needed {
                println!(
                    "  herdr's running server{} is older than the binary; it runs the new one after a restart",
                    s.server_version
                        .as_deref()
                        .map(|v| format!(" ({v})"))
                        .unwrap_or_default()
                );
            }
        }
    }
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
    println!("service: {}", service::status());
    match newer_box {
        Some(v) => println!(
            "version: {} (newer release {v}: `pitupitu update`)",
            update::current()
        ),
        None => println!("version: {}", update::current()),
    }
    Ok(())
}
