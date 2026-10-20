//! Running `pitupitu connect` as a user service: a systemd user unit on
//! Linux, a launchd agent on macOS. The service runs the binary that
//! installed it, with the PATH it was installed from, so it finds herdr
//! and the agents the same way the person's shell does.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

const UNIT: &str = "pitupitu.service";
const LABEL: &str = "chat.pitupitu.box";

pub struct Installed {
    pub file: PathBuf,
    /// How to read its logs.
    pub logs: String,
}

/// The binary the service runs: this one, or its Homebrew `opt` link.
pub fn exe() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("finding this binary")?;
    Ok(match homebrew_opt(&exe) {
        Some(opt) if opt.exists() => opt,
        _ => exe,
    })
}

/// Homebrew runs us from `<prefix>/Cellar/<formula>/<version>/bin/<name>`,
/// a folder `brew upgrade` deletes. Its `<prefix>/opt/<formula>` link
/// follows upgrades, so the service is pointed there instead.
fn homebrew_opt(exe: &Path) -> Option<PathBuf> {
    let name = exe.file_name()?;
    let bin = exe.parent()?;
    let formula_dir = bin.parent()?.parent()?;
    let cellar = formula_dir.parent()?;
    if bin.file_name()? != "bin" || cellar.file_name()? != "Cellar" {
        return None;
    }
    Some(
        cellar
            .parent()?
            .join("opt")
            .join(formula_dir.file_name()?)
            .join("bin")
            .join(name),
    )
}

/// The PATH the service runs with: the installing shell's, plus herdr's
/// folder when herdr was just installed somewhere that is not on it yet.
fn env_path() -> String {
    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into());
    match crate::herdr_box::find_binary()
        .as_deref()
        .and_then(Path::parent)
    {
        Some(dir) if !std::env::split_paths(&path).any(|d| d == dir) => {
            format!("{path}:{}", dir.display())
        }
        _ => path,
    }
}

fn run(cmd: &str, args: &[&str]) -> Result<()> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .with_context(|| format!("running {cmd}"))?;
    if !out.status.success() {
        bail!(
            "{cmd} {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// The config path is passed only when it is not the default one.
fn config_arg(config: &Path) -> Option<PathBuf> {
    (config != crate::config::default_path()).then(|| config.to_path_buf())
}

pub fn systemd_unit(exe: &Path, config: Option<&Path>, path: &str) -> String {
    let mut exec = format!("{} connect", quote_systemd(&exe.display().to_string()));
    if let Some(c) = config {
        exec = format!(
            "{} --config {} connect",
            quote_systemd(&exe.display().to_string()),
            quote_systemd(&c.display().to_string())
        );
    }
    format!(
        "[Unit]
Description=pitupitu box agent: links this machine to pitupitu cloud
After=network-online.target
Wants=network-online.target

[Service]
ExecStart={exec}
Restart=always
RestartSec=5
# Stopping or restarting the service ends the box agent only, not the
# herdr server it may have started: agents running there keep running.
KillMode=process
Environment={env}

[Install]
WantedBy=default.target
",
        env = quote_systemd(&format!("PATH={path}")),
    )
}

fn quote_systemd(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

pub fn launchd_plist(exe: &Path, config: Option<&Path>, path: &str, log: &Path) -> String {
    let mut args = vec![exe.display().to_string()];
    if let Some(c) = config {
        args.extend(["--config".into(), c.display().to_string()]);
    }
    args.push("connect".into());
    let args: String = args
        .iter()
        .map(|a| format!("    <string>{}</string>\n", xml(a)))
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
{args}  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>{path}</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>AbandonProcessGroup</key>
  <true/>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#,
        path = xml(path),
        log = xml(&log.display().to_string()),
    )
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn write(file: &Path, text: &str) -> Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(file, text).with_context(|| format!("writing {}", file.display()))
}

fn uid() -> Result<String> {
    let out = Command::new("id")
        .arg("-u")
        .output()
        .context("running id")?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn systemd_file() -> PathBuf {
    crate::config::home_dir()
        .join(".config/systemd/user")
        .join(UNIT)
}

fn launchd_file() -> PathBuf {
    crate::config::home_dir()
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

/// Write the service and (re)start it, so it runs this binary now.
pub fn install(config: &Path) -> Result<Installed> {
    let exe = exe()?;
    let config = config_arg(config);
    if cfg!(target_os = "macos") {
        let file = launchd_file();
        let log = crate::config::home_dir().join("Library/Logs/pitupitu.log");
        write(
            &file,
            &launchd_plist(&exe, config.as_deref(), &env_path(), &log),
        )?;
        let domain = format!("gui/{}", uid()?);
        let _ = run("launchctl", &["bootout", &format!("{domain}/{LABEL}")]);
        run(
            "launchctl",
            &["bootstrap", &domain, &file.display().to_string()],
        )?;
        Ok(Installed {
            file,
            logs: format!("tail -f {}", log.display()),
        })
    } else if cfg!(target_os = "linux") {
        let file = systemd_file();
        write(&file, &systemd_unit(&exe, config.as_deref(), &env_path()))?;
        run("systemctl", &["--user", "daemon-reload"])?;
        run("systemctl", &["--user", "enable", UNIT])?;
        run("systemctl", &["--user", "restart", UNIT])?;
        Ok(Installed {
            file,
            logs: "journalctl --user -u pitupitu -f".into(),
        })
    } else {
        bail!("no user service support on this system; run `pitupitu connect` yourself")
    }
}

/// Whether a service for the box agent is installed here.
pub fn installed() -> bool {
    if cfg!(target_os = "macos") {
        launchd_file().exists()
    } else if cfg!(target_os = "linux") {
        systemd_file().exists()
    } else {
        false
    }
}

/// Restart the service so it runs the binary now at its path. `false`
/// when no service is installed. Only the box agent restarts: herdr's
/// server is not part of the service, even when the box started it.
pub fn restart() -> Result<bool> {
    if cfg!(target_os = "macos") {
        if !launchd_file().exists() {
            return Ok(false);
        }
        run(
            "launchctl",
            &["kickstart", "-k", &format!("gui/{}/{LABEL}", uid()?)],
        )?;
        Ok(true)
    } else if cfg!(target_os = "linux") {
        if !systemd_file().exists() {
            return Ok(false);
        }
        run("systemctl", &["--user", "restart", UNIT])?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// [`restart`] without waiting for it, for the box agent restarting
/// itself from inside the service: the service manager stops this
/// process and starts the new binary.
pub fn restart_soon() -> Result<bool> {
    if cfg!(target_os = "linux") {
        if !systemd_file().exists() {
            return Ok(false);
        }
        run("systemctl", &["--user", "restart", "--no-block", UNIT])?;
        return Ok(true);
    }
    restart()
}

pub fn uninstall() -> Result<Option<PathBuf>> {
    if cfg!(target_os = "macos") {
        let file = launchd_file();
        let _ = run(
            "launchctl",
            &["bootout", &format!("gui/{}/{LABEL}", uid()?)],
        );
        if file.exists() {
            std::fs::remove_file(&file)?;
            return Ok(Some(file));
        }
    } else if cfg!(target_os = "linux") {
        let file = systemd_file();
        let _ = run("systemctl", &["--user", "disable", "--now", UNIT]);
        if file.exists() {
            std::fs::remove_file(&file)?;
            let _ = run("systemctl", &["--user", "daemon-reload"]);
            return Ok(Some(file));
        }
    }
    Ok(None)
}

/// One line on whether the service is there and running.
pub fn status() -> String {
    if cfg!(target_os = "macos") {
        if !launchd_file().exists() {
            return "not installed".into();
        }
        let running = uid()
            .ok()
            .and_then(|u| {
                Command::new("launchctl")
                    .args(["print", &format!("gui/{u}/{LABEL}")])
                    .output()
                    .ok()
            })
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("state = running"))
            .unwrap_or(false);
        format!(
            "{} ({})",
            if running { "running" } else { "not running" },
            launchd_file().display()
        )
    } else if cfg!(target_os = "linux") {
        if !systemd_file().exists() {
            return "not installed".into();
        }
        let state = Command::new("systemctl")
            .args(["--user", "is-active", UNIT])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "unknown".into());
        format!("{state} ({})", systemd_file().display())
    } else {
        "not supported here".into()
    }
}

/// On Linux with systemd, user services stop at logout and start only at
/// login unless lingering is on. The user it is off for, else `None`.
pub fn linger_off() -> Option<String> {
    if !cfg!(target_os = "linux") || !Path::new("/run/systemd/system").exists() {
        return None;
    }
    let user = std::env::var("USER").ok()?;
    let out = Command::new("loginctl")
        .args(["show-user", &user, "-p", "Linger"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .contains("Linger=no")
        .then_some(user)
}

/// `loginctl enable-linger <user>`.
pub fn enable_linger(user: &str) -> Result<()> {
    run("loginctl", &["enable-linger", user])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_unit_runs_connect_with_the_installing_path() {
        let unit = systemd_unit(
            Path::new("/home/me/.local/bin/pitupitu"),
            None,
            "/home/me/.local/bin:/usr/bin",
        );
        assert!(unit.contains("ExecStart=\"/home/me/.local/bin/pitupitu\" connect\n"));
        assert!(unit.contains("Environment=\"PATH=/home/me/.local/bin:/usr/bin\"\n"));
        let unit = systemd_unit(Path::new("/b/pitupitu"), Some(Path::new("/c/p.toml")), "/x");
        assert!(unit.contains("ExecStart=\"/b/pitupitu\" --config \"/c/p.toml\" connect\n"));
    }

    #[test]
    fn the_plist_escapes_and_orders_arguments() {
        let plist = launchd_plist(
            Path::new("/opt/homebrew/bin/pitupitu"),
            None,
            "/opt/homebrew/bin:/usr/bin",
            Path::new("/Users/me/Library/Logs/pitupitu.log"),
        );
        assert!(plist
            .contains("<string>/opt/homebrew/bin/pitupitu</string>\n    <string>connect</string>"));
        assert!(xml("a<b&c").contains("&lt;") && xml("a<b&c").contains("&amp;"));
    }

    #[test]
    fn a_homebrew_binary_is_run_through_its_opt_link() {
        assert_eq!(
            homebrew_opt(Path::new(
                "/opt/homebrew/Cellar/pitupitu/0.3.3/bin/pitupitu"
            )),
            Some(PathBuf::from("/opt/homebrew/opt/pitupitu/bin/pitupitu"))
        );
        assert_eq!(
            homebrew_opt(Path::new(
                "/home/linuxbrew/.linuxbrew/Cellar/pitupitu/0.3.3_1/bin/pitupitu"
            )),
            Some(PathBuf::from(
                "/home/linuxbrew/.linuxbrew/opt/pitupitu/bin/pitupitu"
            ))
        );
        assert_eq!(
            homebrew_opt(Path::new("/home/me/.local/bin/pitupitu")),
            None
        );
    }
}
