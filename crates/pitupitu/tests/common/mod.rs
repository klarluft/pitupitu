//! Shared by the terminal host tests: a stand-in `claude` and a holder.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::net::UnixStream;

/// Stand-in for Claude Code: hooks via the pitupitu binary, like
/// `--settings` would. It reports that it started, answers each line it
/// reads and reports the turn done, and asks for permission when told
/// to. Like a real agent's TUI, it redraws the screen once a dialog is
/// answered, so the dialog is gone.
pub const FAKE_CLAUDE: &str = r#"#!/bin/sh
hook() { "$FAKE_PITUPITU" hook "$PITUPITU_PANE" "$1" </dev/null >/dev/null 2>&1; }
printf '\033]2;Stand-in session\007'
echo "fake claude ready (args: $*)"
hook start
while IFS= read -r line; do
  hook prompt
  case "$line" in
    ask*) printf 'Do you want to proceed?\n 1. Yes\n 2. No\n'; IFS= read -r answer; printf '\033[2J\033[H'; echo "answered $answer";;
    quit) exit 0;;
    *) echo "answer: $line";;
  esac
  hook stop
done
"#;

/// The holder, stopped when the test ends, passed or not: a holder with
/// terminals open keeps running by design.
pub struct Holder(pub std::process::Child);

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A holder of its own in `dir`, with the stand-in `claude` first on its
/// PATH and a plain shell. Returns the holder, its socket and a project
/// folder to work in.
pub async fn start_holder(dir: &Path) -> (Holder, PathBuf, PathBuf) {
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_pitupitu"));
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    // A login shell would read the profiles and lose the stand-in's PATH.
    let shell = bin.join("testshell");
    std::fs::write(&shell, "#!/bin/sh\nexec /bin/sh\n").unwrap();
    let fake = bin.join("claude");
    std::fs::write(&fake, FAKE_CLAUDE).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        for f in [&fake, &shell] {
            std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let work = dir.join("project");
    std::fs::create_dir_all(&work).unwrap();
    let socket = dir.join("terms.sock");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let holder = Holder(
        std::process::Command::new(&exe)
            .arg("terminals")
            .env("PITUPITU_TERMS_SOCKET", &socket)
            .env("PATH", &path)
            .env("SHELL", &shell)
            .env("FAKE_PITUPITU", &exe)
            .spawn()
            .unwrap(),
    );
    let end = Instant::now() + Duration::from_secs(5);
    while UnixStream::connect(&socket).await.is_err() {
        assert!(Instant::now() < end, "holder did not start");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    (holder, socket, work)
}
