//! The environment herdr's server is started with.
//!
//! Every pane inherits the server's environment. A server the person
//! starts from a terminal gets the terminal's; one the box starts from its
//! service (systemd, launchd) would get the service's, which is much
//! thinner: no variables from the person's login profile, a `PATH` frozen
//! at setup, and on WSL none of what Windows gives a terminal (running
//! Windows programs such as `code` or `explorer.exe`, the Windows folders
//! on `PATH`, WSLg's display and sound). So before starting a server the
//! box rebuilds what a terminal would have:
//!
//! - the person's login shell is asked for its environment
//!   (`$SHELL -l -c 'env -0'`), and what the box's own environment lacks
//!   is taken from it; `PATH` is the box's own followed by the login
//!   shell's entries it does not have yet;
//! - on WSL, the interop socket, the distro's name, the Windows `PATH`
//!   (unless `wsl.conf` turns it off) and WSLg's variables, each only when
//!   missing;
//! - the service manager's own variables and those herdr sets in its panes
//!   are left out.
//!
//! What the box already has always wins: a box agent run from a terminal
//! starts herdr as before. Anything that fails or takes too long is left
//! out, never an error; the server then starts as it did before.

use herdr::ServerEnv;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::debug;

/// Longest wait for the login shell, and for each Windows program asked.
const ASK_LIMIT: Duration = Duration::from_secs(5);
/// Printed before the environment, so whatever a login profile prints
/// first is skipped.
const MARK: &str = "__pitupitu_env__";
/// Set while the login shell is asked, for a profile that wants to skip
/// something slow or interactive.
pub const PROBE_VAR: &str = "PITUPITU_ENV_PROBE";

/// Variables a service manager sets for its own service, and those herdr
/// sets in each pane: never handed to a new server.
const DROP: &[&str] = &[
    // systemd
    "INVOCATION_ID",
    "JOURNAL_STREAM",
    "MANAGERPID",
    "SYSTEMD_EXEC_PID",
    "NOTIFY_SOCKET",
    "LISTEN_PID",
    "LISTEN_FDS",
    "LISTEN_FDNAMES",
    "WATCHDOG_PID",
    "WATCHDOG_USEC",
    "MAINPID",
    // launchd
    "XPC_SERVICE_NAME",
    // herdr, inside a pane
    "HERDR_ENV",
    "HERDR_PANE_ID",
    "HERDR_TAB_ID",
    "HERDR_WORKSPACE_ID",
    "HERDR_SOCKET_PATH",
    "HERDR_BIN_PATH",
];

/// Variables of the login shell's that describe that shell itself or the
/// terminal it ran in, not the person's setup: never taken from it.
const NOT_FROM_LOGIN: &[&str] = &[
    "PWD",
    "OLDPWD",
    "SHLVL",
    "_",
    "TERM",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "COLORTERM",
    // zsh's own, set again by every zsh from its startup files
    "FPATH",
    PROBE_VAR,
];

/// The environment for a herdr server started now.
pub async fn for_herdr() -> ServerEnv {
    let own: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    let login = login_shell_env().await.unwrap_or_default();
    let mut env = merge(&own, &login);
    if is_wsl() {
        wsl(&mut env).await;
    }
    let server = env.into_server_env(&own);
    debug!(
        set = server.set.len(),
        removed = server.remove.len(),
        from_login_shell = !login.is_empty(),
        "herdr's server environment"
    );
    server
}

/// The box's environment as it will be handed on, with what was added.
#[derive(Debug, Default)]
struct Env {
    vars: HashMap<OsString, OsString>,
}

impl Env {
    fn get(&self, k: &str) -> Option<&OsStr> {
        self.vars.get(OsStr::new(k)).map(OsString::as_os_str)
    }

    fn missing(&self, k: &str) -> bool {
        self.get(k).is_none_or(OsStr::is_empty)
    }

    fn set_if_missing(&mut self, k: &str, v: impl Into<OsString>) {
        if self.missing(k) {
            self.vars.insert(k.into(), v.into());
        }
    }

    fn path(&self) -> Vec<PathBuf> {
        self.get("PATH")
            .map(|p| std::env::split_paths(p).collect())
            .unwrap_or_default()
    }

    /// Adds `dirs` to the end of `PATH`, those not on it yet.
    fn append_path(&mut self, dirs: impl IntoIterator<Item = PathBuf>) {
        let mut path = self.path();
        let before = path.len();
        for d in dirs {
            if !d.as_os_str().is_empty() && !path.contains(&d) {
                path.push(d);
            }
        }
        if path.len() != before {
            if let Ok(joined) = std::env::join_paths(path) {
                self.vars.insert("PATH".into(), joined);
            }
        }
    }

    /// What differs from `own`: set or replaced, and removed.
    fn into_server_env(self, own: &[(OsString, OsString)]) -> ServerEnv {
        let own: HashMap<&OsString, &OsString> = own.iter().map(|(k, v)| (k, v)).collect();
        let mut set: Vec<(OsString, OsString)> = self
            .vars
            .iter()
            .filter(|(k, v)| own.get(k) != Some(v))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        set.sort();
        let mut remove: Vec<OsString> = own
            .keys()
            .filter(|k| !self.vars.contains_key(**k))
            .map(|k| (*k).clone())
            .collect();
        remove.sort();
        ServerEnv { set, remove }
    }
}

/// The box's own environment without [`DROP`], plus what the login shell
/// has and it lacks; `PATH` is the box's own, then the login shell's new
/// entries.
fn merge(own: &[(OsString, OsString)], login: &[(OsString, OsString)]) -> Env {
    let dropped = |k: &OsStr| DROP.iter().any(|d| OsStr::new(d) == k);
    let mut env = Env {
        vars: own.iter().filter(|(k, _)| !dropped(k)).cloned().collect(),
    };
    for (k, v) in login {
        let skip = dropped(k)
            || NOT_FROM_LOGIN.iter().any(|n| OsStr::new(n) == k)
            || k.to_string_lossy().starts_with("BASH_FUNC_");
        if skip {
            continue;
        }
        if k == "PATH" {
            env.append_path(std::env::split_paths(v));
        } else if !env.vars.contains_key(k) {
            env.vars.insert(k.clone(), v.clone());
        }
    }
    env
}

/// The person's login shell's environment: `$SHELL -l -c 'env -0'`, with
/// no input and a time limit. `None` when there is no such shell, it
/// fails or it takes too long.
async fn login_shell_env() -> Option<Vec<(OsString, OsString)>> {
    let shell = std::env::var_os("SHELL").filter(|s| !s.is_empty())?;
    let name = Path::new(&shell)
        .file_name()?
        .to_string_lossy()
        .into_owned();
    // csh and tcsh take -l only on its own.
    if name.ends_with("csh") {
        return None;
    }
    let mut cmd = tokio::process::Command::new(&shell);
    cmd.args(["-l", "-c", &format!("echo {MARK}; exec env -0")])
        .env(PROBE_VAR, "1")
        .current_dir(crate::config::home_dir())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let out = match tokio::time::timeout(ASK_LIMIT, cmd.output()).await {
        Ok(Ok(out)) if out.status.success() => out,
        Ok(Ok(out)) => {
            debug!(status = %out.status, "login shell: no environment");
            return None;
        }
        Ok(Err(e)) => {
            debug!("login shell: {e}");
            return None;
        }
        Err(_) => {
            debug!("login shell: took too long");
            return None;
        }
    };
    parse_env(&out.stdout)
}

/// `env -0`'s output after [`MARK`]'s line.
fn parse_env(out: &[u8]) -> Option<Vec<(OsString, OsString)>> {
    let mark = format!("{MARK}\n");
    let start = out.windows(mark.len()).position(|w| w == mark.as_bytes())? + mark.len();
    let vars: Vec<_> = out[start..]
        .split(|b| *b == 0)
        .filter_map(|entry| {
            let eq = entry.iter().position(|b| *b == b'=')?;
            (eq > 0).then(|| (os(&entry[..eq]), os(&entry[eq + 1..])))
        })
        .collect();
    (!vars.is_empty()).then_some(vars)
}

#[cfg(unix)]
fn os(b: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(b.to_vec())
}

#[cfg(not(unix))]
fn os(b: &[u8]) -> OsString {
    String::from_utf8_lossy(b).into_owned().into()
}

// --- WSL ---------------------------------------------------------------

/// Whether this is Linux under WSL, by its interop handler or its run
/// folder.
pub fn is_wsl() -> bool {
    cfg!(target_os = "linux")
        && (Path::new("/run/WSL").is_dir()
            || std::fs::read_dir("/proc/sys/fs/binfmt_misc")
                .map(|d| {
                    d.flatten()
                        .any(|e| e.file_name().to_string_lossy().starts_with("WSLInterop"))
                })
                .unwrap_or(false))
}

/// What `/etc/wsl.conf` says about interop and where Windows drives are.
#[derive(Debug, PartialEq, Eq)]
struct WslConf {
    interop: bool,
    append_windows_path: bool,
    /// Where drives are mounted, ending in `/`: `/mnt/` unless set.
    root: String,
}

impl Default for WslConf {
    fn default() -> Self {
        WslConf {
            interop: true,
            append_windows_path: true,
            root: "/mnt/".into(),
        }
    }
}

fn parse_wsl_conf(text: &str) -> WslConf {
    let mut conf = WslConf::default();
    let mut section = String::new();
    for line in text.lines() {
        let line = line.split(['#', ';']).next().unwrap_or("").trim();
        if let Some(s) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = s.trim().to_ascii_lowercase();
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim().trim_matches('"'));
        let yes = !v.eq_ignore_ascii_case("false") && v != "0";
        match (section.as_str(), k.as_str()) {
            ("interop", "enabled") => conf.interop = yes,
            ("interop", "appendwindowspath") => conf.append_windows_path = yes,
            ("automount", "root") if !v.is_empty() => {
                conf.root = format!("{}/", v.trim_end_matches('/'));
            }
            _ => {}
        }
    }
    conf
}

/// Adds what WSL gives a terminal and a service does not get.
async fn wsl(env: &mut Env) {
    let conf = std::fs::read_to_string("/etc/wsl.conf")
        .map(|t| parse_wsl_conf(&t))
        .unwrap_or_default();
    if env.missing("WSL_DISTRO_NAME") {
        if let Some(name) = distro_name().await {
            env.set_if_missing("WSL_DISTRO_NAME", name);
        }
    }
    if conf.interop {
        let current = env.get("WSL_INTEROP").map(PathBuf::from);
        if current.as_deref().is_none_or(|p| !is_socket(p)) {
            if let Some(sock) = interop_socket() {
                env.vars.insert("WSL_INTEROP".into(), sock.into());
            }
        }
        let has_windows = env.path().iter().any(|d| on_drive(d, &conf.root));
        if conf.append_windows_path && !has_windows {
            if let Some(sock) = env.get("WSL_INTEROP").map(PathBuf::from) {
                if let Some(path) = windows_path(&conf.root, &sock).await {
                    env.append_path(path);
                }
            }
        }
    }
    let gpu = PathBuf::from("/usr/lib/wsl/lib");
    if gpu.is_dir() {
        env.append_path([gpu]);
    }
    // WSLg: X11, Wayland and sound for Linux GUI apps.
    if Path::new("/mnt/wslg").is_dir() {
        if Path::new("/tmp/.X11-unix/X0").exists() {
            env.set_if_missing("DISPLAY", ":0");
        }
        let runtime = env
            .get("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| "/mnt/wslg/runtime-dir".into());
        if runtime.join("wayland-0").exists() {
            env.set_if_missing("WAYLAND_DISPLAY", "wayland-0");
        }
        if Path::new("/mnt/wslg/PulseServer").exists() {
            env.set_if_missing("PULSE_SERVER", "unix:/mnt/wslg/PulseServer");
        }
    }
}

#[cfg(unix)]
fn is_socket(p: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::metadata(p)
        .map(|m| m.file_type().is_socket())
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_socket(_: &Path) -> bool {
    false
}

/// A WSL interop socket that answers: WSL's own (`1_interop`, which
/// lives as long as the distro) first, then the newest of the others.
fn interop_socket() -> Option<PathBuf> {
    let dir = Path::new("/run/WSL");
    let mut others: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with("_interop"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    others.sort_by_key(|o| std::cmp::Reverse(o.0));
    std::iter::once(dir.join("1_interop"))
        .chain(others.into_iter().map(|(_, p)| p))
        .find(|p| answers(p))
}

#[cfg(unix)]
fn answers(p: &Path) -> bool {
    is_socket(p) && std::os::unix::net::UnixStream::connect(p).is_ok()
}

#[cfg(not(unix))]
fn answers(_: &Path) -> bool {
    false
}

/// Whether `dir` is on a Windows drive (`/mnt/c/...`).
fn on_drive(dir: &Path, root: &str) -> bool {
    dir.to_str()
        .and_then(|d| d.strip_prefix(root))
        .is_some_and(|rest| {
            let mut c = rest.chars();
            matches!(c.next(), Some(l) if l.is_ascii_alphabetic())
                && matches!(c.next(), None | Some('/'))
        })
}

/// The distro's name, from its Windows path (`\\wsl.localhost\Ubuntu\`).
async fn distro_name() -> Option<String> {
    let out = run_limited(
        tokio::process::Command::new("wslpath")
            .args(["-w", "/"])
            .env_remove("WSL_INTEROP"),
    )
    .await?;
    parse_distro(&out)
}

fn parse_distro(out: &str) -> Option<String> {
    let rest = out.trim().strip_prefix(r"\\")?;
    let (_host, rest) = rest.split_once('\\')?;
    let name = rest.trim_end_matches('\\').split('\\').next()?;
    (!name.is_empty()).then(|| name.to_string())
}

/// Windows's `PATH` as Linux folders, asked from `cmd.exe` through the
/// interop socket `sock`.
async fn windows_path(root: &str, sock: &Path) -> Option<Vec<PathBuf>> {
    let drives = std::fs::read_dir(root).ok()?;
    let mut found: Vec<PathBuf> = drives
        .flatten()
        .map(|e| e.path().join("Windows/System32/cmd.exe"))
        .filter(|p| p.is_file())
        .collect();
    found.sort();
    let cmd_exe = found.into_iter().next()?;
    // A Windows folder to start in: from a Linux one cmd.exe complains
    // about UNC paths before it answers.
    let drive = cmd_exe.ancestors().nth(3)?.to_path_buf();
    let out = run_limited(
        tokio::process::Command::new(&cmd_exe)
            .args(["/d", "/c", "echo %PATH%"])
            .env("WSL_INTEROP", sock)
            .current_dir(drive),
    )
    .await?;
    let dirs = windows_to_linux(&out, root);
    (!dirs.is_empty()).then_some(dirs)
}

/// `C:\Program Files\x;D:\y` as `<root>c/Program Files/x`, `<root>d/y`;
/// what is not a drive path is left out.
fn windows_to_linux(path: &str, root: &str) -> Vec<PathBuf> {
    path.trim()
        .split(';')
        .filter_map(|e| {
            let e = e.trim().trim_end_matches('\\');
            let mut c = e.chars();
            let letter = c.next().filter(char::is_ascii_alphabetic)?;
            if c.next() != Some(':') {
                return None;
            }
            let rest = c.as_str().replace('\\', "/");
            if rest.contains('%') || (!rest.is_empty() && !rest.starts_with('/')) {
                return None;
            }
            Some(PathBuf::from(format!(
                "{root}{}{rest}",
                letter.to_ascii_lowercase()
            )))
        })
        .fold(Vec::new(), |mut v, d| {
            if !v.contains(&d) {
                v.push(d);
            }
            v
        })
}

/// A short command's output, or `None` when it fails or takes too long.
async fn run_limited(cmd: &mut tokio::process::Command) -> Option<String> {
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    match tokio::time::timeout(ASK_LIMIT, cmd.output()).await {
        Ok(Ok(out)) if out.status.success() => {
            Some(String::from_utf8_lossy(&out.stdout).replace('\r', ""))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(list: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        list.iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect()
    }

    #[test]
    fn the_login_shell_fills_in_what_the_service_lacks() {
        let own = vars(&[
            ("HOME", "/home/me"),
            ("LANG", "C.UTF-8"),
            ("PATH", "/home/me/.local/bin:/usr/bin"),
            ("INVOCATION_ID", "abc"),
            ("JOURNAL_STREAM", "9:1"),
        ]);
        let login = vars(&[
            ("HOME", "/home/me"),
            ("LANG", "en_US.UTF-8"),
            ("PATH", "/opt/tool/bin:/usr/bin:/home/me/.local/bin"),
            ("JAVA_HOME", "/opt/jdk"),
            ("PWD", "/home/me"),
            ("SHLVL", "1"),
            ("TERM", "dumb"),
            ("BASH_FUNC_x%%", "() { :; }"),
            (PROBE_VAR, "1"),
        ]);
        let env = merge(&own, &login).into_server_env(&own);
        assert_eq!(
            env.set,
            vars(&[
                ("JAVA_HOME", "/opt/jdk"),
                ("PATH", "/home/me/.local/bin:/usr/bin:/opt/tool/bin"),
            ])
        );
        assert_eq!(
            env.remove,
            vec![OsString::from("INVOCATION_ID"), "JOURNAL_STREAM".into()]
        );
    }

    #[test]
    fn a_box_run_from_a_terminal_changes_nothing_but_herdrs_pane_variables() {
        let own = vars(&[
            ("PATH", "/usr/bin"),
            ("TERM", "xterm-256color"),
            ("HERDR_ENV", "1"),
            ("HERDR_PANE_ID", "w1:p1"),
        ]);
        let env = merge(&own, &own).into_server_env(&own);
        assert!(env.set.is_empty(), "{env:?}");
        assert_eq!(
            env.remove,
            vec![OsString::from("HERDR_ENV"), "HERDR_PANE_ID".into()]
        );
    }

    #[test]
    fn env_output_is_read_after_the_mark() {
        let mut out = b"Welcome!\nsome motd\n".to_vec();
        out.extend(format!("{MARK}\n").as_bytes());
        out.extend(b"A=1\0MULTI=line one\nline two\0EMPTY=\0=bad\0NOEQ\0");
        let got = parse_env(&out).unwrap();
        assert_eq!(
            got,
            vars(&[("A", "1"), ("MULTI", "line one\nline two"), ("EMPTY", "")])
        );
        assert_eq!(parse_env(b"no mark here\0A=1\0"), None);
    }

    #[test]
    fn wsl_conf_is_read_like_wsl_reads_it() {
        assert_eq!(parse_wsl_conf(""), WslConf::default());
        let conf = parse_wsl_conf(
            "[boot]\nsystemd=true\n\n[interop]\nenabled = true # on\nappendWindowsPath=false\n\
             [automount]\nroot = /win/\n",
        );
        assert_eq!(
            conf,
            WslConf {
                interop: true,
                append_windows_path: false,
                root: "/win/".into(),
            }
        );
        assert!(!parse_wsl_conf("[interop]\nenabled=false").interop);
        assert_eq!(parse_wsl_conf("[automount]\nroot=/\n").root, "/");
    }

    #[test]
    fn windows_path_becomes_drive_folders() {
        let path = "C:\\Windows\\system32;C:\\Program Files\\Microsoft VS Code\\bin\\;\
                    D:\\tools;%SystemRoot%\\x;\\\\server\\share;;C:\\Windows\\system32;E:\\";
        assert_eq!(
            windows_to_linux(path, "/mnt/"),
            vec![
                PathBuf::from("/mnt/c/Windows/system32"),
                "/mnt/c/Program Files/Microsoft VS Code/bin".into(),
                "/mnt/d/tools".into(),
                "/mnt/e".into(),
            ]
        );
        assert_eq!(windows_to_linux("C:\\x", "/"), vec![PathBuf::from("/c/x")]);
    }

    #[test]
    fn drive_folders_are_told_from_linux_ones() {
        assert!(on_drive(Path::new("/mnt/c/Windows"), "/mnt/"));
        assert!(on_drive(Path::new("/mnt/d"), "/mnt/"));
        assert!(!on_drive(Path::new("/mnt/wslg/x"), "/mnt/"));
        assert!(!on_drive(Path::new("/usr/bin"), "/mnt/"));
    }

    #[test]
    fn the_distro_name_comes_from_its_windows_path() {
        assert_eq!(
            parse_distro("\\\\wsl.localhost\\Ubuntu\\\n").as_deref(),
            Some("Ubuntu")
        );
        assert_eq!(
            parse_distro("\\\\wsl$\\Ubuntu-24.04\\").as_deref(),
            Some("Ubuntu-24.04")
        );
        assert_eq!(parse_distro("C:\\"), None);
    }

    #[test]
    fn path_additions_keep_order_and_skip_repeats() {
        let mut env = Env::default();
        env.vars.insert("PATH".into(), "/a:/b".into());
        env.append_path([PathBuf::from("/b"), "/c".into(), "".into(), "/a".into()]);
        assert_eq!(env.get("PATH"), Some(OsStr::new("/a:/b:/c")));
    }
}
