//! The coding agents ("harnesses") this box looks for, and what it learns
//! about each (`docs/box-link.md`, "Harnesses"): which command herdr runs
//! for a kind, which fixed lister tells its models and sign-in, and the
//! screen phrases it shows. The cloud sends the current profiles in a
//! `harnesses` note; until one arrives, and when the cloud is out of reach,
//! the copy built in here (or the last one received) is used.
//!
//! What runs: `<command> --version` for each profile, and the listers
//! below, each a fixed command of this file's own with a fixed set of
//! arguments. A profile only picks which lister applies; it can never make
//! the box run anything else. Settings files are read for model ids and
//! names only; nothing else in them is parsed or sent, and credentials are
//! never read.

use crate::inventory::Available;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, RwLock};
use std::time::Duration;
use tokio::io::AsyncReadExt;

/// One lister run may take this long.
const LISTER_TIMEOUT: Duration = Duration::from_secs(10);
/// A lister's output past this is not read.
const OUTPUT_MAX: usize = 1024 * 1024;
/// Models sent per harness.
const MODELS_MAX: usize = 200;
/// Where the last profiles from the cloud are kept.
const STATE_FILE: &str = "harnesses.json";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Dismiss {
    pub phrases: Vec<String>,
    pub keys: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Screens {
    pub trust: Vec<String>,
    pub trust_yes: Vec<String>,
    pub approval: Vec<String>,
    pub login: Vec<String>,
    pub model_unavailable: Vec<String>,
    pub dismiss: Vec<Dismiss>,
}

/// What the box uses of a profile; other fields the cloud sends are kept
/// out of it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub kind: String,
    /// The plain name people know it by ("Kiro CLI").
    pub name: Option<String>,
    pub command: String,
    pub lister: Option<String>,
    pub integration: Option<String>,
    pub screens: Screens,
}

impl Profile {
    fn new(kind: &str, command: &str, lister: Option<&str>, integration: Option<&str>) -> Self {
        Profile {
            kind: kind.into(),
            name: None,
            command: command.into(),
            lister: lister.map(str::to_string),
            integration: integration.map(str::to_string),
            screens: Screens::default(),
        }
    }
}

impl Profile {
    /// Its plain name, else its kind.
    pub fn name(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| match self.kind.as_str() {
                "claude" => "Claude Code".into(),
                "codex" => "Codex".into(),
                "kiro" => "Kiro CLI".into(),
                "opencode" => "OpenCode".into(),
                "kilo" => "Kilo Code".into(),
                "qwen" => "Qwen Code".into(),
                "cline" => "Cline".into(),
                "agy" => "Antigravity".into(),
                "gemini" => "Gemini CLI".into(),
                k => k.to_string(),
            })
    }
}

/// The profiles as of this release.
pub fn built_in() -> Vec<Profile> {
    let p = Profile::new;
    let mut list = vec![
        p("claude", "claude", Some("claude"), Some("claude")),
        p("codex", "codex", Some("codex"), Some("codex")),
        p("gemini", "gemini", None, None),
        p("agy", "agy", Some("agy"), Some("antigravity-cli")),
        p("opencode", "opencode", Some("opencode"), Some("opencode")),
        p("kilo", "kilo", Some("kilo"), Some("kilo")),
        p("pi", "pi", Some("pi"), Some("pi")),
        p("qwen", "qwen", Some("qwen_settings"), Some("qwen")),
        p("cline", "cline", Some("cline_settings"), None),
        p("kiro", "kiro-cli", Some("kiro"), None),
        p("cursor", "cursor-agent", None, Some("cursor")),
        p("copilot", "copilot", None, Some("copilot")),
        p("droid", "droid", None, Some("droid")),
        p("amp", "amp", None, None),
        p("grok", "grok", None, Some("grok")),
        p("hermes", "hermes", None, Some("hermes")),
        p("kimi", "kimi", None, Some("kimi")),
        p("letta", "letta", None, Some("letta")),
        p("qodercli", "qodercli", None, Some("qodercli")),
        p("devin", "devin", None, Some("devin")),
        p("omp", "omp", None, Some("omp")),
        p("mastracode", "mastracode", None, Some("mastracode")),
        p("maki", "maki", None, None),
        p("muse", "muse", None, None),
    ];
    for profile in &mut list {
        match profile.kind.as_str() {
            "kiro" => {
                profile.screens = Screens {
                    trust: vec!["Trust this workspace?".into()],
                    trust_yes: vec!["Trust, continue".into()],
                    approval: vec!["requires approval".into()],
                    login: vec!["let's get you signed in".into()],
                    ..Screens::default()
                }
            }
            "cline" => {
                profile.screens.dismiss = vec![Dismiss {
                    phrases: vec!["ClinePass".into()],
                    keys: vec!["esc".into()],
                }]
            }
            _ => {}
        }
    }
    list
}

static PROFILES: RwLock<Option<Vec<Profile>>> = RwLock::new(None);

/// The profiles in use: the cloud's last ones, else the built-in copy.
pub fn profiles() -> Vec<Profile> {
    PROFILES
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(built_in)
}

/// A command the box will look for: a bare name, no path.
pub fn command_ok(command: &str) -> bool {
    let mut chars = command.chars();
    command.len() <= 32
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
}

/// Use `list` from now on: kept in `state_dir` for the next start, and its
/// screen phrases handed to the dialog detection. Profiles with a command
/// that is not a bare name are dropped, and so are dismiss keys other than
/// Esc.
pub fn apply(list: Vec<Profile>, state_dir: Option<&Path>) {
    let list: Vec<Profile> = list
        .into_iter()
        .filter(|p| command_ok(&p.command) && p.kind.parse::<herdr::AgentKind>().is_ok())
        .map(|mut p| {
            p.screens
                .dismiss
                .retain(|d| d.keys.iter().all(|k| k == "esc"));
            p
        })
        .collect();
    herdr::detect::set_screens(screens_of(&list));
    if let Some(dir) = state_dir {
        if let Ok(text) = serde_json::to_vec(&list) {
            let _ = std::fs::create_dir_all(dir);
            let tmp = dir.join(format!("{STATE_FILE}.tmp"));
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, dir.join(STATE_FILE));
            }
        }
    }
    *PROFILES.write().unwrap_or_else(|e| e.into_inner()) = Some(list);
}

/// The `harnesses` note from the cloud.
pub fn apply_note(note: &Value, state_dir: Option<&Path>) -> bool {
    match note
        .get("profiles")
        .cloned()
        .map(serde_json::from_value::<Vec<Profile>>)
    {
        Some(Ok(list)) if !list.is_empty() => {
            apply(list, state_dir);
            true
        }
        _ => false,
    }
}

/// At start: the last profiles the cloud sent, else the built-in copy.
pub fn load(state_dir: &Path) {
    let saved = std::fs::read(state_dir.join(STATE_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<Profile>>(&b).ok())
        .filter(|l| !l.is_empty());
    match saved {
        Some(list) => apply(list, None),
        None => herdr::detect::set_screens(screens_of(&built_in())),
    }
}

/// Every profile's phrases together: the screen is seen before the kind is
/// known, and a phrase of one harness does not appear on another's screen.
fn screens_of(list: &[Profile]) -> herdr::detect::Screens {
    let mut s = herdr::detect::Screens::default();
    for p in list {
        s.trust.extend(p.screens.trust.iter().cloned());
        s.trust_yes.extend(p.screens.trust_yes.iter().cloned());
        s.approval.extend(p.screens.approval.iter().cloned());
        s.login.extend(p.screens.login.iter().cloned());
        for d in &p.screens.dismiss {
            s.dismiss.extend(d.phrases.iter().cloned());
        }
    }
    s
}

/// A model a harness offers here.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Model {
    pub id: String,
    pub name: Option<String>,
    pub provider: Option<String>,
    pub note: Option<String>,
    pub default: bool,
}

/// What a lister found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Listed {
    pub signed_in: Option<bool>,
    pub models: Option<Vec<Model>>,
    /// RFC 3339, UTC.
    pub at: Option<String>,
    pub error: Option<String>,
}

static LISTED: Mutex<Option<HashMap<String, Listed>>> = Mutex::new(None);

/// Told when listers ran, so the link sends the new `available`.
pub static CHANGED: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// The last lister result for `kind`, if any ran.
pub fn listed(kind: &str) -> Option<Listed> {
    LISTED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(kind).cloned())
}

/// Run the listers of the harnesses found here (or only `kind`'s) and
/// keep what they said; returns the `available` entries refreshed.
/// `None` when `kind` names no profile.
pub async fn refresh(kind: Option<&str>, home: &Path) -> Option<Vec<Available>> {
    let list = profiles();
    if let Some(k) = kind {
        if !list.iter().any(|p| p.kind == k) {
            return None;
        }
    }
    let found = crate::inventory::available().await;
    let mut out = Vec::new();
    for a in found {
        if kind.is_some_and(|k| k != a.kind) {
            continue;
        }
        let Some(p) = list.iter().find(|p| p.kind == a.kind) else {
            continue;
        };
        let l = run_lister(p, home).await;
        LISTED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(HashMap::new)
            .insert(a.kind.clone(), l);
        out.push(crate::inventory::with_listed(a));
    }
    CHANGED.notify_one();
    Some(out)
}

/// Refresh every lister now and then every `every`, for as long as the
/// process runs.
pub fn spawn_refresher(home: PathBuf, every: Duration) {
    tokio::spawn(async move {
        loop {
            refresh(None, &home).await;
            tokio::time::sleep(every).await;
        }
    });
}

async fn run_lister(p: &Profile, home: &Path) -> Listed {
    let at = Some(now_rfc3339());
    let done =
        |models: Option<Vec<Model>>, signed_in: Option<bool>, error: Option<String>| Listed {
            signed_in,
            models: models.map(|mut m| {
                m.truncate(MODELS_MAX);
                m
            }),
            at: at.clone(),
            error,
        };
    let cmd = p.command.as_str();
    match p.lister.as_deref() {
        // Claude Code takes its aliases and Anthropic's ids: the cloud
        // knows them; nothing to run.
        Some("claude") | None => done(None, None, None),
        Some("codex") => match output(cmd, &["debug", "models"]).await {
            Ok(o) => parsed(codex_models(&o), None),
            Err(e) => done(None, None, Some(e)),
        },
        Some("kiro") => match output(cmd, &["whoami"]).await {
            Ok(o) if signed_out(&o) => done(None, Some(false), None),
            Ok(_) => match output(cmd, &["chat", "--list-models", "--format", "json"]).await {
                Ok(o) => parsed(kiro_models(&o), Some(true)),
                Err(e) => done(None, Some(true), Some(e)),
            },
            // `whoami` exits non-zero when nobody is signed in.
            Err(_) => done(None, Some(false), None),
        },
        Some(l @ ("opencode" | "kilo")) => match output(cmd, &["models"]).await {
            Ok(o) => parsed(Some(slash_models(&o)), None),
            Err(e) => done(None, None, Some(format!("{l}: {e}"))),
        },
        Some("pi") => match output(cmd, &["--list-models"]).await {
            Ok(o) => parsed(Some(pi_models(&o)), None),
            Err(e) => done(None, None, Some(e)),
        },
        Some("agy") => match output(cmd, &["models"]).await {
            Ok(o) => parsed(Some(tab_models(&o)), None),
            Err(e) => done(None, None, Some(e)),
        },
        Some("qwen_settings") => {
            let text = std::fs::read_to_string(home.join(".qwen/settings.json")).ok();
            parsed(text.as_deref().and_then(qwen_models), None)
        }
        Some("cline_settings") => {
            let text = std::fs::read_to_string(home.join(".cline/data/settings/models.json")).ok();
            parsed(text.as_deref().and_then(cline_models), None)
        }
        Some(_) => done(None, None, None),
    }
}

fn parsed(models: Option<Vec<Model>>, signed_in: Option<bool>) -> Listed {
    let at = Some(now_rfc3339());
    match models {
        Some(mut m) => {
            m.truncate(MODELS_MAX);
            Listed {
                signed_in,
                models: Some(m),
                at,
                error: None,
            }
        }
        None => Listed {
            signed_in,
            models: None,
            at,
            error: Some("its model list could not be read".into()),
        },
    }
}

fn signed_out(whoami: &str) -> bool {
    let w = whoami.to_lowercase();
    w.contains("not logged in") || w.contains("not signed in")
}

/// Run `cmd args…` with nothing to read from, no terminal and no colour;
/// its standard output, at most [`OUTPUT_MAX`], or why it failed.
async fn output(cmd: &str, args: &[&str]) -> Result<String, String> {
    let mut child = tokio::process::Command::new(cmd)
        .args(args)
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "not found".to_string())?;
    let mut stdout = child.stdout.take().ok_or("no output")?;
    let read = async {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 64 * 1024];
        loop {
            let n = stdout.read(&mut chunk).await.map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.len() > OUTPUT_MAX {
                return Err("its model list is too long".to_string());
            }
        }
        let status = child.wait().await.map_err(|e| e.to_string())?;
        if status.success() {
            Ok(String::from_utf8_lossy(&buf).into_owned())
        } else {
            Err(format!("it exited with {status}"))
        }
    };
    tokio::time::timeout(LISTER_TIMEOUT, read)
        .await
        .unwrap_or_else(|_| Err("it took over 10 seconds".into()))
}

fn now_rfc3339() -> String {
    crate::inventory::rfc3339(std::time::SystemTime::now())
}

fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// `codex debug models`: its catalog, the hidden ones left out.
pub fn codex_models(text: &str) -> Option<Vec<Model>> {
    let v: Value = serde_json::from_str(text).ok()?;
    Some(
        v.get("models")?
            .as_array()?
            .iter()
            .filter(|m| m.get("visibility").and_then(Value::as_str) != Some("hide"))
            .filter_map(|m| {
                Some(Model {
                    id: s(m, "slug")?,
                    name: s(m, "display_name"),
                    provider: Some("openai".into()),
                    ..Model::default()
                })
            })
            .collect(),
    )
}

/// `kiro-cli chat --list-models --format json`.
pub fn kiro_models(text: &str) -> Option<Vec<Model>> {
    let v: Value = serde_json::from_str(text).ok()?;
    let default = s(&v, "default_model");
    Some(
        v.get("models")?
            .as_array()?
            .iter()
            .filter_map(|m| {
                let id = s(m, "model_id")?;
                let note = m
                    .get("rate_multiplier")
                    .and_then(Value::as_f64)
                    .map(|r| format!("{r:.1}x credits"));
                Some(Model {
                    default: default.as_deref() == Some(id.as_str()),
                    name: s(m, "model_name").filter(|n| *n != id),
                    note,
                    id,
                    provider: None,
                })
            })
            .collect(),
    )
}

/// One `provider/model` per line (OpenCode, Kilo Code).
pub fn slash_models(text: &str) -> Vec<Model> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.contains(char::is_whitespace) && l.contains('/'))
        .map(|l| Model {
            id: l.to_string(),
            provider: l.split('/').next().map(str::to_string),
            ..Model::default()
        })
        .collect()
}

/// `pi --list-models`: a table whose first two columns are the provider
/// and the model.
pub fn pi_models(text: &str) -> Vec<Model> {
    text.lines()
        .filter_map(|l| {
            let mut cols = l.split_whitespace();
            let (provider, model) = (cols.next()?, cols.next()?);
            (provider != "provider").then(|| Model {
                id: format!("{provider}/{model}"),
                provider: Some(provider.to_string()),
                ..Model::default()
            })
        })
        .collect()
}

/// `agy models`: `id<TAB>name` per line.
pub fn tab_models(text: &str) -> Vec<Model> {
    text.lines()
        .filter_map(|l| {
            let (id, name) = l.split_once('\t')?;
            let id = id.trim();
            (!id.is_empty() && !id.contains(' ')).then(|| Model {
                id: id.to_string(),
                name: Some(name.trim().to_string()).filter(|n| !n.is_empty()),
                ..Model::default()
            })
        })
        .collect()
}

/// Qwen Code's settings: `modelProviders.<provider>[].id` and `name`, the
/// one in `model.name` its default. Nothing else in the file is read.
pub fn qwen_models(text: &str) -> Option<Vec<Model>> {
    let v: Value = serde_json::from_str(text).ok()?;
    let default = v.get("model").and_then(|m| s(m, "name"));
    let providers = v.get("modelProviders")?.as_object()?;
    Some(
        providers
            .iter()
            .flat_map(|(provider, list)| {
                list.as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|m| {
                        let id = s(m, "id")?;
                        Some(Model {
                            default: default.as_deref() == Some(id.as_str()),
                            name: s(m, "name"),
                            provider: Some(provider.clone()),
                            id,
                            note: None,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .collect(),
    )
}

/// Cline's model list: `providers.<provider>.models.<id>`. Its provider
/// settings and tokens live in other files, which are never read.
pub fn cline_models(text: &str) -> Option<Vec<Model>> {
    let v: Value = serde_json::from_str(text).ok()?;
    let providers = v.get("providers")?.as_object()?;
    Some(
        providers
            .iter()
            .flat_map(|(provider, p)| {
                p.get("models")
                    .and_then(Value::as_object)
                    .into_iter()
                    .flat_map(|m| m.keys())
                    .map(|id| Model {
                        id: id.clone(),
                        provider: Some(provider.clone()),
                        ..Model::default()
                    })
                    .collect::<Vec<_>>()
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_profiles_name_only_bare_commands() {
        for p in built_in() {
            assert!(command_ok(&p.command), "{}", p.command);
            assert!(p.kind.parse::<herdr::AgentKind>().is_ok(), "{}", p.kind);
        }
        assert!(!command_ok("/bin/sh"));
        assert!(!command_ok("../kiro"));
        assert!(!command_ok("rm -rf"));
        assert!(command_ok("kiro-cli"));
    }

    #[test]
    fn the_cli_takes_every_built_in_kind() {
        let kinds: Vec<String> = built_in().into_iter().map(|p| p.kind).collect();
        assert_eq!(kinds, crate::settings::AGENT_KINDS);
    }

    #[test]
    fn a_note_replaces_the_profiles_and_drops_what_is_unsafe() {
        let dir = tempfile::tempdir().unwrap();
        let note = serde_json::json!({"note": "harnesses", "revision": "r1", "profiles": [
            {"kind": "kiro", "name": "Kiro CLI", "command": "kiro-cli", "lister": "kiro",
             "screens": {"dismiss": [{"phrases": ["Ad"], "keys": ["esc"]}, {"phrases": ["X"], "keys": ["enter"]}]}},
            {"kind": "evil", "command": "/bin/sh", "lister": null}
        ]});
        assert!(apply_note(&note, Some(dir.path())));
        let list = profiles();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].command, "kiro-cli");
        assert_eq!(list[0].screens.dismiss.len(), 1);
        // Kept for the next start.
        *PROFILES.write().unwrap() = None;
        load(dir.path());
        assert_eq!(profiles()[0].kind, "kiro");
        *PROFILES.write().unwrap() = None;
        assert!(!apply_note(&serde_json::json!({"note": "harnesses"}), None));
    }

    #[test]
    fn listers_read_each_harness_format() {
        let codex = r#"{"models":[{"slug":"gpt-6.1-sol","display_name":"GPT-6.1-Sol","base_instructions":"…"},{"slug":"hidden","visibility":"hide"}]}"#;
        let m = codex_models(codex).unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].id, "gpt-6.1-sol");
        assert_eq!(m[0].name.as_deref(), Some("GPT-6.1-Sol"));

        let kiro = r#"{"models":[{"model_name":"auto","model_id":"auto","rate_multiplier":1.0},{"model_name":"claude-opus-5.5","model_id":"claude-opus-5.5","rate_multiplier":2.0}],"default_model":"auto"}"#;
        let m = kiro_models(kiro).unwrap();
        assert!(m[0].default);
        assert_eq!(m[1].note.as_deref(), Some("2.0x credits"));
        assert_eq!(m[1].name, None);

        let oc =
            "opencode/big-pickle\nollama/orcarouter/Qwen3.8-27B:q5_K_M\n\nWarning: something\n";
        let m = slash_models(oc);
        assert_eq!(m.len(), 2);
        assert_eq!(m[1].provider.as_deref(), Some("ollama"));

        let pi = "provider  model                context  max-out\nollama    qwen3:27b            32.8K    8.2K\nanthropic claude-opus-5-5      1M       64K\n";
        let m = pi_models(pi);
        assert_eq!(m.len(), 2);
        assert_eq!(m[1].id, "anthropic/claude-opus-5-5");

        let agy = "Fetching available models...\ngemini-3.8-flash-high\tGemini 3.8 Flash (High)\nclaude-opus-4-6-thinking\tClaude Opus 4.6 (Thinking)\n";
        let m = tab_models(agy);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].name.as_deref(), Some("Gemini 3.8 Flash (High)"));

        let qwen = r#"{"env":{"OPENAI_API_KEY":"never read"},"model":{"name":"q1"},"modelProviders":{"openai":[{"id":"q1","name":"Qwen One","envKey":"X"}]}}"#;
        let m = qwen_models(qwen).unwrap();
        assert_eq!(m.len(), 1);
        assert!(m[0].default);
        assert!(!serde_json::to_string(&m).unwrap().contains("never read"));

        let cline =
            r#"{"version":1,"providers":{"ollama":{"models":{"qwen3:27b":{"contextWindow":1}}}}}"#;
        let m = cline_models(cline).unwrap();
        assert_eq!(m[0].id, "qwen3:27b");
        assert_eq!(m[0].provider.as_deref(), Some("ollama"));

        assert!(signed_out("Not logged in"));
        assert!(!signed_out("Logged in with Builder ID"));
    }
}
