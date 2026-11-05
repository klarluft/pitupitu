//! Work an agent left running after its turn, seen from its processes.
//!
//! herdr counts an agent back at its prompt as ready whatever it left
//! running, and many agents show nothing about such a job on screen. What
//! every one of them leaves behind is a process: everything started in a
//! herdr pane inherits `HERDR_PANE_ID`, even after `nohup` or `setsid`.
//!
//! Not every process of a pane is a job. One counts when it
//!
//! - belongs to this box's herdr session (`HERDR_SESSION`): pane ids repeat
//!   between sessions;
//! - started during a turn, or at any time when the box first sees the
//!   pane: whatever started before that turn is the shell's or the agent's;
//! - no longer has the pane's terminal: an agent's helpers (MCP and language
//!   servers) mostly stay in its terminal session, while the commands it
//!   runs get a session of their own, and a job is what outlives them;
//! - does not answer on its standard input and output: that is a helper
//!   the agent started in a session of its own, such as Kiro's server;
//! - is not a daemon a tool leaves behind on purpose, which can idle for
//!   hours (`HELPERS`).
//!
//! macOS hides the environment of Apple's own programs (`sleep`, `/bin/zsh`,
//! `/bin/bash`), so a process that shows none there belongs to the pane its
//! parents lead to. One left with no parent (`nohup … &`) is taken by its
//! working folder, if it started during a turn: two agents in one folder
//! can both be flagged. An agent that sets its process title (pi) loses its
//! environment there too, so its pane may show no process at all.
//!
//! The rest of a process's environment is read past and never kept; only
//! a yes or no per pane leaves the box, as `background` in the agent list.

use herdr::AgentInfo;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// While a job runs, how often its pane is looked at again.
const LOOK_EVERY: Duration = Duration::from_secs(3);
/// `ps` tells a process's age in whole seconds.
const START_SLACK: Duration = Duration::from_secs(1);

/// Programs that stay behind by design and are nobody's unfinished work.
/// A name with "daemon" in the command line needs no entry.
const HELPERS: &[&str] = &[
    "adb",
    "bazel",
    "caffeinate",
    "chrome_crashpad_handler",
    "crashpad_handler",
    "dirmngr",
    "gopls",
    "gpg-agent",
    "keyboxd",
    "rust-analyzer",
    "sccache",
    "ssh-agent",
    "tsserver",
    "watchman",
];

/// A process started in a herdr pane.
#[derive(Debug, Clone, PartialEq)]
pub struct Proc {
    pub pane: String,
    /// `HERDR_SESSION`; absent in herdr's default session.
    pub session: Option<String>,
    pub started: SystemTime,
    /// Its controlling terminal, if it has one.
    pub tty: Option<String>,
    /// Its command line, arguments joined by spaces.
    pub command: String,
    /// Its standard input and output are both a pipe or a socket.
    pub talks: bool,
    /// macOS only: no pane could be told (`pane` is empty), and this is
    /// the working folder of a parentless process.
    pub cwd: Option<String>,
}

fn is_helper(command: &str) -> bool {
    let name = command.split_whitespace().next().unwrap_or("");
    let name = name.rsplit('/').next().unwrap_or(name);
    if HELPERS.contains(&name) || command.contains("[mux]") {
        return true;
    }
    command.to_lowercase().contains("daemon")
}

#[derive(Debug)]
struct Pane {
    seq: u64,
    /// A job counts from here on: the last time the pane was seen at rest.
    since: SystemTime,
    running: bool,
    /// Due for a look the next time it is ready.
    look: bool,
}

/// What the box knows of the jobs in each pane. It outlives a link, so a
/// reconnect does not lose a job.
pub struct Jobs {
    session: Option<String>,
    panes: HashMap<String, Pane>,
    looked: Option<Instant>,
    read: Reader,
}

/// Reads the machine's processes. The time is how far back a process
/// that can only be told by its folder is of interest.
type Reader = fn(SystemTime) -> Vec<Proc>;

impl Jobs {
    /// For the herdr session `session` (`None`: the default one).
    pub fn new(session: Option<String>) -> Self {
        Self::reading(session, processes)
    }

    /// Reading the machine's processes with `read`.
    pub fn reading(session: Option<String>, read: Reader) -> Self {
        Jobs {
            session,
            panes: HashMap::new(),
            looked: None,
            read,
        }
    }

    /// Take in the latest list; whether the processes need reading now.
    fn plan(&mut self, agents: &[AgentInfo], now: SystemTime) -> bool {
        self.panes
            .retain(|id, _| agents.iter().any(|a| a.pane_id.0 == *id));
        for a in agents {
            let ready = a.agent_status.is_ready();
            let Some(p) = self.panes.get_mut(&a.pane_id.0) else {
                self.panes.insert(
                    a.pane_id.0.clone(),
                    Pane {
                        seq: a.state_change_seq,
                        since: SystemTime::UNIX_EPOCH,
                        running: false,
                        look: true,
                    },
                );
                continue;
            };
            let changed = p.seq != a.state_change_seq;
            p.seq = a.state_change_seq;
            p.look |= changed;
            if ready && !changed && !p.look && !p.running {
                p.since = now;
            }
        }
        let ready = |id: &String| {
            agents
                .iter()
                .any(|a| a.pane_id.0 == *id && a.agent_status.is_ready())
        };
        let turn_ended = self.panes.iter().any(|(id, p)| p.look && ready(id));
        let job_running = self.panes.iter().any(|(id, p)| p.running && ready(id));
        turn_ended || (job_running && self.looked.is_none_or(|t| t.elapsed() >= LOOK_EVERY))
    }

    /// The start of the oldest turn a job is being looked for in.
    fn oldest_turn(&self, now: SystemTime) -> SystemTime {
        self.panes
            .values()
            .map(|p| p.since)
            .filter(|s| *s != SystemTime::UNIX_EPOCH)
            .min()
            .unwrap_or(now)
    }

    /// Judge the ready panes from what `procs` says.
    fn judge(&mut self, agents: &[AgentInfo], procs: &[Proc]) {
        for a in agents.iter().filter(|a| a.agent_status.is_ready()) {
            let Some(p) = self.panes.get_mut(&a.pane_id.0) else {
                continue;
            };
            let mine: Vec<&Proc> = procs
                .iter()
                .filter(|x| x.pane == a.pane_id.0 && x.session == self.session)
                .collect();
            // The pane's first process with a terminal is its shell, or the
            // agent when it took the shell's place.
            let shell = mine
                .iter()
                .filter(|x| x.tty.is_some())
                .min_by_key(|x| x.started);
            // Parentless and told by its folder: only ever a turn's work,
            // which a box that has just started knows nothing of.
            let in_folder = |x: &Proc| {
                p.since != SystemTime::UNIX_EPOCH
                    && x.started >= p.since - START_SLACK
                    && !x.talks
                    && !is_helper(&x.command)
                    && x.cwd
                        .as_ref()
                        .is_some_and(|c| Path::new(c).starts_with(&a.cwd))
            };
            let orphan = procs.iter().any(in_folder);
            p.running = orphan
                || shell.is_some_and(|shell| {
                    let from = p.since.max(shell.started) - START_SLACK;
                    mine.iter().any(|x| {
                        x.tty != shell.tty
                            && x.started >= from
                            && !x.talks
                            && !is_helper(&x.command)
                    })
                });
            p.look = false;
        }
        self.looked = Some(Instant::now());
    }

    /// Say on each agent whether it has a job running.
    fn mark(&self, agents: &mut [AgentInfo]) {
        for a in agents {
            a.background = a.agent_status.is_ready()
                && self.panes.get(&a.pane_id.0).is_some_and(|p| p.running);
        }
    }
}

/// Bring `jobs` up to date with `agents` and mark them.
pub async fn update(jobs: &Arc<Mutex<Jobs>>, agents: &mut [AgentInfo]) {
    let now = SystemTime::now();
    let read = {
        let mut j = jobs.lock().unwrap();
        j.plan(agents, now).then(|| (j.read, j.oldest_turn(now)))
    };
    if let Some((read, since)) = read {
        let procs = tokio::task::spawn_blocking(move || read(since))
            .await
            .unwrap_or_default();
        jobs.lock().unwrap().judge(agents, &procs);
    }
    jobs.lock().unwrap().mark(agents);
}

/// Every process of this user that was started in a herdr pane.
#[cfg(target_os = "linux")]
pub fn processes(_since: SystemTime) -> Vec<Proc> {
    let Some(uptime) = std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
    else {
        return Vec::new();
    };
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let now = SystemTime::now();
    dir.flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .bytes()
                .all(|b| b.is_ascii_digit())
        })
        .filter_map(|e| {
            // Another user's process cannot be read, and is not ours.
            let env = std::fs::read(e.path().join("environ")).ok()?;
            let (pane, session) = herdr_vars(&env)?;
            let stat = std::fs::read_to_string(e.path().join("stat")).ok()?;
            let (tty, age) = read_stat(&stat, uptime)?;
            let command = std::fs::read(e.path().join("cmdline")).ok()?;
            Some(Proc {
                pane,
                session,
                started: now.checked_sub(age)?,
                tty,
                command: String::from_utf8_lossy(&command).replace('\0', " "),
                talks: [0, 1].iter().all(|fd| {
                    std::fs::read_link(e.path().join("fd").join(fd.to_string()))
                        .is_ok_and(|to| is_stream(&to.to_string_lossy()))
                }),
                cwd: None,
            })
        })
        .collect()
}

/// A `/proc/<pid>/fd` link to a pipe or a socket.
#[cfg(any(target_os = "linux", test))]
fn is_stream(link: &str) -> bool {
    link.starts_with("pipe:") || link.starts_with("socket:")
}

/// `HERDR_PANE_ID` and `HERDR_SESSION` of a `/proc/<pid>/environ`.
#[cfg(any(target_os = "linux", test))]
fn herdr_vars(env: &[u8]) -> Option<(String, Option<String>)> {
    let var = |name: &[u8]| {
        env.split(|b| *b == 0)
            .find_map(|e| e.strip_prefix(name))
            .map(|v| String::from_utf8_lossy(v).into_owned())
    };
    Some((var(b"HERDR_PANE_ID=")?, var(b"HERDR_SESSION=")))
}

/// The terminal and age of a `/proc/<pid>/stat`, given the uptime. A
/// process name can hold anything, so fields count from its closing `)`.
#[cfg(any(target_os = "linux", test))]
fn read_stat(stat: &str, uptime: f64) -> Option<(Option<String>, Duration)> {
    /// `USER_HZ`, the unit of a start time: 100 on every Linux we run on.
    const TICKS: f64 = 100.0;
    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    let tty = (*fields.get(4)? != "0").then(|| fields[4].to_string());
    let started: f64 = fields.get(19)?.parse().ok()?;
    Some((
        tty,
        Duration::from_secs_f64((uptime - started / TICKS).max(0.0)),
    ))
}

/// Every process of this user that was started in a herdr pane, and the
/// parentless ones since `since` whose environment is hidden. Without `-a`,
/// `ps -x` lists one's own processes.
#[cfg(target_os = "macos")]
pub fn processes(since: SystemTime) -> Vec<Proc> {
    use std::process::Command;
    let out = Command::new("/bin/ps")
        .args(["-xwwE", "-o", "pid=,ppid=,etime=,tty=,command="])
        .output();
    let Ok(out) = out else {
        return Vec::new();
    };
    let now = SystemTime::now();
    let rows: Vec<Row> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| ps_line(l, now))
        .collect();
    let mut procs = from_rows(&rows, since);
    // Only what lost its terminal needs asking about; mostly nothing does.
    let pids: Vec<String> = procs
        .iter()
        .filter(|(_, p)| p.tty.is_none())
        .map(|(pid, _)| pid.to_string())
        .collect();
    if !pids.is_empty() {
        let out = Command::new("/usr/sbin/lsof")
            .args(["-a", "-d", "cwd,0,1", "-Fpftn", "-p", &pids.join(",")])
            .output();
        if let Ok(out) = out {
            let open = open_files(&String::from_utf8_lossy(&out.stdout));
            for (pid, p) in &mut procs {
                let Some(o) = open.get(pid) else { continue };
                p.talks = o.streams == 2;
                if p.pane.is_empty() {
                    p.cwd = o.cwd.clone();
                }
            }
        }
    }
    procs.into_iter().map(|(_, p)| p).collect()
}

/// A line of `ps`.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, PartialEq)]
struct Row {
    pid: u32,
    ppid: u32,
    started: SystemTime,
    tty: Option<String>,
    command: String,
    /// `HERDR_PANE_ID` and `HERDR_SESSION`, when the environment shows.
    herdr: Option<(String, Option<String>)>,
}

/// The processes of interest among `rows`: those with a pane, their own or
/// their nearest parent's, and the parentless without a terminal since
/// `since`, with an empty pane.
#[cfg(any(target_os = "macos", test))]
fn from_rows(rows: &[Row], since: SystemTime) -> Vec<(u32, Proc)> {
    let by_pid: HashMap<u32, &Row> = rows.iter().map(|r| (r.pid, r)).collect();
    let pane_of = |row: &Row| {
        let mut at = Some(row);
        for _ in 0..64 {
            let r = at?;
            if r.herdr.is_some() {
                return r.herdr.clone();
            }
            at = by_pid.get(&r.ppid).copied();
        }
        None
    };
    rows.iter()
        .filter_map(|r| {
            let (pane, session) = match pane_of(r) {
                Some(found) => found,
                None if r.ppid == 1 && r.tty.is_none() && r.started >= since => {
                    (String::new(), None)
                }
                None => return None,
            };
            let proc = Proc {
                pane,
                session,
                started: r.started,
                tty: r.tty.clone(),
                command: r.command.clone(),
                talks: false,
                cwd: None,
            };
            Some((r.pid, proc))
        })
        .collect()
}

/// A line of `ps -E -o pid=,ppid=,etime=,tty=,command=`: the command line
/// runs into the environment, with nothing between them but a space.
#[cfg(any(target_os = "macos", test))]
fn ps_line(line: &str, now: SystemTime) -> Option<Row> {
    let (pid, line) = line.trim_start().split_once(char::is_whitespace)?;
    let (ppid, line) = line.trim_start().split_once(char::is_whitespace)?;
    let (age, line) = line.trim_start().split_once(char::is_whitespace)?;
    let (tty, line) = line.trim_start().split_once(char::is_whitespace)?;
    let line = line.trim_start();
    let var = |name: &str| {
        line.split(' ')
            .find_map(|w| w.strip_prefix(name))
            .map(str::to_string)
    };
    // A word like NAME= ends the command line; `--flag=x` is not one.
    let end = line
        .split(' ')
        .scan(0, |at, w| {
            let start = *at;
            *at += w.len() + 1;
            Some((start, w))
        })
        .find(|(_, w)| {
            w.split_once('=').is_some_and(|(k, _)| {
                !k.is_empty() && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            })
        })
        .map_or(line.len(), |(at, _)| at);
    Some(Row {
        pid: pid.parse().ok()?,
        ppid: ppid.parse().ok()?,
        started: now.checked_sub(etime(age)?)?,
        tty: (tty != "??").then(|| tty.to_string()),
        command: line[..end].trim_end().to_string(),
        herdr: var("HERDR_PANE_ID=").map(|pane| (pane, var("HERDR_SESSION="))),
    })
}

/// What `lsof` says of a process.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Default, PartialEq)]
struct Open {
    /// How many of descriptors 0 and 1 are a pipe or a socket.
    streams: u8,
    cwd: Option<String>,
}

/// From `lsof -Fpftn`.
#[cfg(any(target_os = "macos", test))]
fn open_files(lsof: &str) -> HashMap<u32, Open> {
    let mut open: HashMap<u32, Open> = HashMap::new();
    let mut pid = 0;
    let mut fd = "";
    for line in lsof.lines() {
        let (kind, value) = line.split_at(line.len().min(1));
        match kind {
            "p" => pid = value.parse().unwrap_or(0),
            "f" => fd = value,
            "t" if matches!(value, "PIPE" | "FIFO" | "unix" | "IPv4" | "IPv6") => {
                let o = open.entry(pid).or_default();
                o.streams += u8::from(fd == "0" || fd == "1");
            }
            "n" if fd == "cwd" => open.entry(pid).or_default().cwd = Some(value.to_string()),
            _ => {}
        }
    }
    open
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn processes(_since: SystemTime) -> Vec<Proc> {
    Vec::new()
}

/// `ps`'s elapsed time, `[[dd-]hh:]mm:ss`.
#[cfg(any(target_os = "macos", test))]
fn etime(s: &str) -> Option<Duration> {
    let (days, clock) = match s.split_once('-') {
        Some((d, c)) => (d.parse::<u64>().ok()?, c),
        None => (0, s),
    };
    let secs = clock
        .split(':')
        .try_fold(0, |n: u64, p| Some(n * 60 + p.parse::<u64>().ok()?))?;
    Some(Duration::from_secs(days * 86_400 + secs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testhost::agent;
    use herdr::AgentStatus::{Done, Idle, Working};

    const PANE: &str = "w1:p1";

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 + secs)
    }

    fn proc(started: u64, tty: Option<&str>, command: &str) -> Proc {
        Proc {
            pane: PANE.into(),
            session: None,
            started: at(started),
            tty: tty.map(str::to_string),
            command: command.into(),
            talks: false,
            cwd: None,
        }
    }

    /// One pass of `update`, at `now`, with `procs` on the machine.
    /// Returns whether the processes were read, and the flag.
    fn tick(
        jobs: &mut Jobs,
        status: herdr::AgentStatus,
        seq: u64,
        now: u64,
        procs: &[Proc],
    ) -> (bool, bool) {
        let mut agents = vec![agent("a", PANE, status, seq)];
        let due = jobs.plan(&agents, at(now));
        if due {
            jobs.judge(&agents, procs);
        }
        jobs.mark(&mut agents);
        (due, agents[0].background)
    }

    fn jobs() -> Jobs {
        Jobs::reading(None, |_| Vec::new())
    }

    /// The agent, from before the turn, with an MCP server beside it.
    fn agent_procs() -> Vec<Proc> {
        vec![
            proc(0, Some("34838"), "opencode"),
            proc(1, Some("34838"), "python3 mcp.py"),
        ]
    }

    #[test]
    fn a_job_counts_until_it_ends() {
        let mut j = jobs();
        let at_rest = agent_procs();
        assert_eq!(tick(&mut j, Idle, 1, 10, &at_rest), (true, false));
        assert_eq!(tick(&mut j, Idle, 1, 11, &at_rest), (false, false));
        tick(&mut j, Working, 2, 12, &at_rest);
        let mut running = agent_procs();
        running.push(proc(13, None, "sleep 240"));
        assert_eq!(tick(&mut j, Done, 3, 14, &running), (true, true));
        // Nothing changes in herdr when the job ends.
        j.looked = None;
        assert_eq!(tick(&mut j, Done, 3, 15, &running), (true, true));
        j.looked = None;
        assert_eq!(tick(&mut j, Done, 3, 18, &at_rest), (true, false));
    }

    #[test]
    fn a_ready_pane_with_nothing_running_is_not_read_again() {
        let mut j = jobs();
        tick(&mut j, Done, 1, 10, &agent_procs());
        for now in 11..20 {
            assert_eq!(tick(&mut j, Done, 1, now, &agent_procs()), (false, false));
        }
    }

    #[test]
    fn a_turn_too_short_to_be_seen_working_still_counts() {
        let mut j = jobs();
        tick(&mut j, Done, 1, 10, &agent_procs());
        tick(&mut j, Done, 1, 11, &agent_procs());
        let mut running = agent_procs();
        running.push(proc(12, None, "sleep 240"));
        assert_eq!(tick(&mut j, Done, 3, 12, &running), (true, true));
    }

    #[test]
    fn what_keeps_the_panes_terminal_is_a_helper_not_a_job() {
        let mut j = jobs();
        tick(&mut j, Idle, 1, 10, &agent_procs());
        tick(&mut j, Idle, 1, 11, &agent_procs());
        tick(&mut j, Working, 2, 12, &agent_procs());
        let mut after = agent_procs();
        after.push(proc(
            13,
            Some("34838"),
            "typescript-language-server --stdio",
        ));
        assert_eq!(tick(&mut j, Done, 3, 14, &after), (true, false));
    }

    #[test]
    fn a_daemon_a_tool_left_behind_is_not_a_job() {
        for command in [
            "/usr/bin/gpg-agent --homedir x",
            "sccache",
            "java -cp x org.gradle.launcher.daemon.bootstrap.GradleDaemon 8.1",
            "ssh: /tmp/x [mux]",
        ] {
            let mut j = jobs();
            tick(&mut j, Idle, 1, 10, &agent_procs());
            let mut after = agent_procs();
            after.push(proc(11, None, command));
            assert_eq!(
                tick(&mut j, Done, 2, 12, &after),
                (true, false),
                "{command}"
            );
        }
    }

    /// Kiro's server: a session of its own, spoken to on stdin and stdout.
    #[test]
    fn a_helper_in_its_own_session_is_not_a_job() {
        let mut j = jobs();
        tick(&mut j, Idle, 1, 10, &agent_procs());
        let mut after = agent_procs();
        let mut server = proc(11, None, "node acp-server.js --transport=stdio");
        server.talks = true;
        after.push(server);
        assert_eq!(tick(&mut j, Done, 2, 12, &after), (true, false));
    }

    #[test]
    fn a_job_from_an_earlier_turn_survives_the_next_turn() {
        let mut j = jobs();
        tick(&mut j, Idle, 1, 10, &agent_procs());
        let mut running = agent_procs();
        running.push(proc(11, None, "sleep 240"));
        assert!(tick(&mut j, Done, 2, 12, &running).1);
        tick(&mut j, Working, 3, 20, &running);
        assert_eq!(tick(&mut j, Done, 4, 30, &running), (true, true));
    }

    #[test]
    fn only_this_herdr_session_counts() {
        let mut other = proc(11, None, "sleep 240");
        other.session = Some("mine".into());
        let mut procs = agent_procs();
        procs.push(other);
        let mut j = jobs();
        tick(&mut j, Idle, 1, 10, &agent_procs());
        assert_eq!(tick(&mut j, Done, 2, 12, &procs), (true, false));
    }

    #[test]
    fn what_ran_before_the_turn_is_not_the_turns_work() {
        let mut j = jobs();
        let mut procs = agent_procs();
        procs.push(proc(5, None, "sleep 240"));
        tick(&mut j, Idle, 1, 10, &agent_procs());
        tick(&mut j, Idle, 1, 20, &agent_procs());
        assert_eq!(tick(&mut j, Done, 2, 21, &procs), (true, false));
    }

    #[test]
    fn a_box_that_restarts_finds_the_job_again() {
        let mut procs = agent_procs();
        procs.push(proc(5, None, "sleep 240"));
        assert_eq!(tick(&mut jobs(), Done, 9, 30, &procs), (true, true));
    }

    #[test]
    fn a_leftover_from_before_the_pane_is_not_found_by_a_new_box() {
        let mut procs = agent_procs();
        for p in &mut procs {
            p.started += Duration::from_secs(100);
        }
        procs.push(proc(5, None, "sleep 240"));
        assert_eq!(tick(&mut jobs(), Done, 9, 130, &procs), (true, false));
    }

    #[test]
    fn a_working_agent_is_not_flagged() {
        let mut j = jobs();
        let mut procs = agent_procs();
        procs.push(proc(5, None, "sleep 240"));
        tick(&mut j, Done, 1, 10, &procs);
        assert_eq!(tick(&mut j, Working, 2, 11, &procs), (false, false));
    }

    #[test]
    fn a_pane_gone_is_forgotten() {
        let mut j = jobs();
        tick(&mut j, Idle, 1, 10, &agent_procs());
        j.plan(&[], at(11));
        assert!(j.panes.is_empty());
    }

    #[test]
    fn the_flag_is_sent_only_when_set() {
        let mut a = agent("a", PANE, Done, 1);
        assert!(serde_json::to_value(&a)
            .unwrap()
            .get("background")
            .is_none());
        a.background = true;
        assert_eq!(serde_json::to_value(&a).unwrap()["background"], true);
    }

    #[test]
    fn linux_environment_and_stat_are_read() {
        let env = b"PATH=/bin\0HERDR_PANE_ID=wA:p1\0HERDR_SESSION=s\0KEY=secret\0";
        assert_eq!(
            herdr_vars(env),
            Some(("wA:p1".to_string(), Some("s".to_string())))
        );
        assert_eq!(herdr_vars(b"PATH=/bin\0"), None);
        let stat = "12 (a) b (c) S 1 2 3 34838 5 0 0 0 0 0 0 0 0 0 20 0 1 0 1000 0 0";
        let (tty, age) = read_stat(stat, 25.0).unwrap();
        assert_eq!(tty.as_deref(), Some("34838"));
        assert_eq!(age, Duration::from_secs(15));
        let none = "12 (a) S 1 2 3 0 5 0 0 0 0 0 0 0 0 0 20 0 1 0 1000 0 0";
        assert_eq!(read_stat(none, 25.0).unwrap().0, None);
    }

    #[test]
    fn a_macos_ps_line_is_read() {
        let now = at(100);
        let line = "  42   1 01:05 ??       sleep 240 --x=1 TERM=xterm HERDR_PANE_ID=wA:p1 \
                    HERDR_SESSION=s PATH=/a b:/c";
        let r = ps_line(line, now).unwrap();
        assert_eq!((r.pid, r.ppid), (42, 1));
        assert_eq!(r.herdr, Some(("wA:p1".to_string(), Some("s".to_string()))));
        assert_eq!(r.tty, None);
        assert_eq!(r.command, "sleep 240 --x=1");
        assert_eq!(r.started, at(35));
        let shell = ps_line("7 1 1-02:00:00 ttys003 -zsh HERDR_PANE_ID=w1:p1", now).unwrap();
        assert_eq!(shell.tty.as_deref(), Some("ttys003"));
        assert_eq!(shell.herdr, Some(("w1:p1".to_string(), None)));
        assert_eq!(ps_line("8 1 00:01 ?? sleep 41", now).unwrap().herdr, None);
    }

    fn row(pid: u32, ppid: u32, started: u64, tty: Option<&str>, pane: Option<&str>) -> Row {
        Row {
            pid,
            ppid,
            started: at(started),
            tty: tty.map(str::to_string),
            command: format!("p{pid}"),
            herdr: pane.map(|p| (p.to_string(), None)),
        }
    }

    /// `/bin/zsh` and `sleep` under Kilo Code show no environment.
    #[test]
    fn a_hidden_process_takes_its_parents_pane() {
        let rows = [
            row(10, 2, 0, Some("ttys1"), Some(PANE)),
            row(11, 10, 20, None, None),
            row(12, 11, 20, None, None),
        ];
        let procs = from_rows(&rows, at(15));
        assert_eq!(procs.len(), 3);
        assert!(procs.iter().all(|(_, p)| p.pane == PANE));
    }

    #[test]
    fn only_recent_parentless_processes_are_of_interest() {
        let rows = [
            row(20, 1, 5, None, None),
            row(21, 1, 20, None, None),
            row(22, 1, 20, Some("ttys1"), None),
            row(23, 9, 20, None, None),
        ];
        let procs = from_rows(&rows, at(15));
        assert_eq!(procs.iter().map(|(pid, _)| *pid).collect::<Vec<_>>(), [21]);
        assert_eq!(procs[0].1.pane, "");
    }

    fn orphan(started: u64, cwd: &str) -> Proc {
        let mut p = proc(started, None, "sleep 41");
        p.pane = String::new();
        p.cwd = Some(cwd.into());
        p
    }

    /// `nohup sleep 41 &` on macOS: no parent, no environment to read.
    #[test]
    fn a_parentless_process_counts_in_the_agents_folder_during_a_turn() {
        // `agent()` works in /tmp.
        let cases = [
            (orphan(11, "/tmp"), true),
            (orphan(11, "/tmp/sub"), true),
            (orphan(11, "/tmpfoo"), false),
            (orphan(11, "/other"), false),
            (orphan(5, "/tmp"), false),
        ];
        for (job, expected) in cases {
            let mut j = jobs();
            tick(&mut j, Idle, 1, 9, &agent_procs());
            tick(&mut j, Idle, 1, 10, &agent_procs());
            let mut procs = agent_procs();
            procs.push(job.clone());
            assert_eq!(tick(&mut j, Done, 2, 12, &procs).1, expected, "{job:?}");
        }
    }

    /// pi on macOS: setting its title wiped its environment.
    #[test]
    fn a_parentless_process_counts_when_the_agent_itself_is_hidden() {
        let mut j = jobs();
        tick(&mut j, Idle, 1, 9, &[]);
        tick(&mut j, Idle, 1, 10, &[]);
        assert!(tick(&mut j, Done, 2, 12, &[orphan(11, "/tmp")]).1);
    }

    #[test]
    fn a_box_just_started_goes_by_the_pane_alone() {
        let mut procs = agent_procs();
        procs.push(orphan(5, "/tmp"));
        assert_eq!(tick(&mut jobs(), Done, 9, 30, &procs), (true, false));
    }

    #[test]
    fn the_oldest_turn_bounds_the_search() {
        let mut j = jobs();
        assert_eq!(j.oldest_turn(at(50)), at(50));
        tick(&mut j, Idle, 1, 9, &agent_procs());
        assert_eq!(j.oldest_turn(at(50)), at(50));
        tick(&mut j, Idle, 1, 10, &agent_procs());
        assert_eq!(j.oldest_turn(at(50)), at(10));
    }

    #[test]
    fn lsof_says_who_talks_and_where() {
        let out = "p10\nfcwd\ntDIR\nn/work\nf0\ntPIPE\nn->x\nf1\ntunix\nn->y\n\
                   p11\nf0\ntCHR\nn/dev/null\nf1\ntPIPE\nn->z\n";
        let open = open_files(out);
        assert_eq!(open[&10].streams, 2);
        assert_eq!(open[&10].cwd.as_deref(), Some("/work"));
        assert_eq!(open[&11].streams, 1);
    }

    #[test]
    fn linux_streams_are_told_from_files() {
        assert!(is_stream("socket:[12]") && is_stream("pipe:[3]"));
        assert!(!is_stream("/dev/null") && !is_stream("/dev/pts/2"));
    }

    #[test]
    fn elapsed_times_are_read() {
        assert_eq!(etime("05:01"), Some(Duration::from_secs(301)));
        assert_eq!(etime("01:00:00"), Some(Duration::from_secs(3600)));
        assert_eq!(etime("2-00:00:01"), Some(Duration::from_secs(172_801)));
        assert_eq!(etime("x"), None);
    }
}
