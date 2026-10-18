//! The account's settings from this box: `pitupitu prefs`, `box`,
//! `default` and `settings`, over `GET`/`PATCH /v1/settings` with this
//! box's key. The preferences are what the person tells pitupitu about
//! themselves and their machines; the voice foreman reads them in every
//! conversation. `note`, `driver` and on/off (`enabled`) are this box's
//! own; `updates` is what the foreman brings up between turns unasked.

use crate::config::Config;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::{IsTerminal, Read};
use std::path::Path;

pub const DRIVERS: &[&str] = &["auto", "herdr", "terminal"];
pub const UPDATES: &[&str] = &["mine", "all", "quiet"];
/// The agents a default can name; `auto` clears it.
pub const AGENTS: &[&str] = &["claude", "codex", "gemini", "agy", "auto"];
pub const AGENT_KINDS: &[&str] = &["claude", "codex", "gemini", "agy"];

/// `auto` is no default at all.
pub fn agent_or_null(name: &str) -> Value {
    if name == "auto" {
        Value::Null
    } else {
        json!(name)
    }
}

fn endpoint(cfg: &Config) -> Result<(String, String)> {
    let Some(key) = cfg.api_key.clone() else {
        bail!("this box is not signed in; run `pitupitu setup`");
    };
    Ok((
        format!("{}/v1/settings", cfg.cloud_url.trim_end_matches('/')),
        key,
    ))
}

async fn answer(resp: reqwest::Response) -> Result<Value> {
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let msg = body["error"]["message"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| body.to_string());
        if status.as_u16() == 401 {
            bail!("pitupitu cloud rejected this box's key; run `pitupitu setup --relink`");
        }
        bail!("pitupitu cloud said {status}: {msg}");
    }
    Ok(body)
}

pub async fn get(cfg: &Config) -> Result<Value> {
    let (url, key) = endpoint(cfg)?;
    let resp = reqwest::Client::new()
        .get(&url)
        .bearer_auth(key)
        .send()
        .await
        .with_context(|| format!("reaching {url}"))?;
    answer(resp).await
}

/// Change some settings; `null` clears one. Returns them all as they now are.
pub async fn patch(cfg: &Config, change: Value) -> Result<Value> {
    let (url, key) = endpoint(cfg)?;
    let resp = reqwest::Client::new()
        .patch(&url)
        .bearer_auth(key)
        .json(&change)
        .send()
        .await
        .with_context(|| format!("reaching {url}"))?;
    answer(resp).await
}

/// Print the settings: JSON, or a few lines for a person.
pub fn print(settings: &Value, as_json: bool) {
    if as_json {
        println!("{settings:#}");
        return;
    }
    let me = settings["box"].as_str().unwrap_or("");
    match settings["preferences"].as_str() {
        Some(p) => println!(
            "preferences: {} line{} (`pitupitu prefs` shows them)",
            p.lines().count(),
            if p.lines().count() == 1 { "" } else { "s" }
        ),
        None => println!("preferences: none (`pitupitu prefs edit` to write some)"),
    }
    println!(
        "default box: {}",
        settings["default_box"]
            .as_str()
            .unwrap_or("none (pitupitu asks)")
    );
    println!(
        "updates: {}",
        match settings["updates"].as_str().unwrap_or("mine") {
            "all" => "all (your own sessions too)",
            "quiet" => "quiet (nothing unless asked)",
            _ => "mine (only what pitupitu started)",
        }
    );
    let models: Vec<String> = settings["agent_models"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| v.as_str().map(|m| format!("{k} on {m}")))
        .collect();
    println!(
        "agent: {}{}",
        settings["default_agent"]
            .as_str()
            .unwrap_or("auto (what the machine has, Claude first)"),
        if models.is_empty() {
            String::new()
        } else {
            format!("; models: {}", models.join(", "))
        }
    );
    println!(
        "voice: {}; patience: {} ms",
        settings["voice"].as_str().unwrap_or("default"),
        settings["patience_ms"]
    );
    println!("boxes:");
    for b in settings["boxes"].as_array().into_iter().flatten() {
        let name = b["name"].as_str().unwrap_or("?");
        let mut line = format!(
            "  {name}{}: {}{}, driver {}",
            if name == me { " (this box)" } else { "" },
            if b["online"] == true {
                "online"
            } else {
                "offline"
            },
            if b["enabled"] == false {
                ", switched off"
            } else {
                ""
            },
            b["driver"].as_str().unwrap_or("auto")
        );
        if let Some(a) = b["agent"].as_str() {
            line.push_str(&format!(", agent {a}"));
        }
        if let Some(n) = b["note"].as_str() {
            line.push_str(&format!(" — {n}"));
        }
        println!("{line}");
    }
    print_places(settings);
}

/// The named folders, one line each (nothing when there are none).
pub fn print_places(settings: &Value) {
    let places: Vec<&Value> = settings["places"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    if places.is_empty() {
        return;
    }
    let me = settings["box"].as_str().unwrap_or("");
    println!("named folders:");
    for p in places {
        let b = p["box"].as_str().unwrap_or("?");
        println!(
            "  {} = {}{}",
            p["name"].as_str().unwrap_or("?"),
            p["path"].as_str().unwrap_or("?"),
            if b == me {
                String::new()
            } else {
                format!(" (on {b})")
            }
        );
    }
}

/// The preferences text from a file, or from stdin for `-`.
pub fn read_text(file: &Path) -> Result<String> {
    if file == Path::new("-") {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        return Ok(s);
    }
    std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))
}

/// `pitupitu prefs edit`: the preferences in `$VISUAL`/`$EDITOR`, saved
/// when they changed.
pub async fn edit(cfg: &Config) -> Result<Option<Value>> {
    if !std::io::stdin().is_terminal() {
        bail!("`pitupitu prefs edit` needs a terminal; use `pitupitu prefs set <file>` (or `-` for stdin)");
    }
    let current = get(cfg).await?;
    let before = current["preferences"].as_str().unwrap_or("").to_string();
    let dir = std::env::temp_dir().join(format!("pitupitu-prefs-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let file = dir.join("preferences.md");
    let template = if before.is_empty() {
        "# What pitupitu should know about you and your machines.\n\
         # Lines starting with # are dropped. Example:\n\
         # - Client work (Acme) goes on the work laptop; everything else here.\n\
         # - I use Codex for reviews and Claude for the rest.\n"
            .to_string()
    } else {
        before.clone()
    };
    std::fs::write(&file, &template)?;
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(&file)
        .status()
        .with_context(|| format!("running {editor}"))?;
    let text = std::fs::read_to_string(&file)?;
    let _ = std::fs::remove_dir_all(&dir);
    if !status.success() {
        bail!("{editor} exited with {status}; nothing saved");
    }
    let text = clean(&text);
    if text == before.trim() {
        return Ok(None);
    }
    Ok(Some(
        patch(cfg, json!({"preferences": text_or_null(&text)})).await?,
    ))
}

/// Drop `#` comment lines and surrounding blank space.
pub fn clean(text: &str) -> String {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

pub fn text_or_null(text: &str) -> Value {
    let t = text.trim();
    if t.is_empty() {
        Value::Null
    } else {
        json!(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_and_blank_edges_are_dropped() {
        assert_eq!(
            clean("# help\n\n- Acme goes on the laptop.\n# more\n- Codex for reviews.\n\n"),
            "- Acme goes on the laptop.\n- Codex for reviews."
        );
        assert_eq!(text_or_null("  \n"), Value::Null);
        assert_eq!(text_or_null(" x "), json!("x"));
    }
}
