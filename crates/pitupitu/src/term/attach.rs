//! `pitupitu attach [session]`: a session of the terminal host, live in
//! this terminal. What you type goes to it; `Ctrl-\` detaches and leaves it
//! running.

use super::socket_path;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::IsTerminal;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// `Ctrl-\`.
const DETACH: u8 = 0x1c;

/// The terminal in raw mode until dropped.
struct Raw(libc::termios);

impl Raw {
    fn enter() -> Result<Raw> {
        // SAFETY: plain termios calls on stdin with a zeroed struct to fill.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut t) != 0 {
                bail!("stdin is not a terminal");
            }
            let saved = t;
            libc::cfmakeraw(&mut t);
            libc::tcsetattr(0, libc::TCSANOW, &t);
            Ok(Raw(saved))
        }
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        // SAFETY: restores what `enter` read.
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &self.0);
        }
    }
}

fn size() -> (u16, u16) {
    // SAFETY: TIOCGWINSZ fills a winsize.
    unsafe {
        let mut w: libc::winsize = std::mem::zeroed();
        if libc::ioctl(1, libc::TIOCGWINSZ, &mut w) == 0 && w.ws_row > 0 {
            (w.ws_row, w.ws_col)
        } else {
            (40, 120)
        }
    }
}

pub async fn attach(target: &str) -> Result<()> {
    if !std::io::stdin().is_terminal() {
        bail!("`pitupitu attach` needs a terminal");
    }
    let stream = UnixStream::connect(socket_path())
        .await
        .context("no sessions: the terminal holder is not running")?;
    let (read, mut write) = stream.into_split();
    let (rows, cols) = size();
    let mut req = json!({"op": "attach", "agent": target, "rows": rows, "cols": cols}).to_string();
    req.push('\n');
    write.write_all(req.as_bytes()).await?;
    let mut from = BufReader::new(read);
    let mut line = String::new();
    from.read_line(&mut line).await?;
    let reply: Value = serde_json::from_str(&line).context("terminal holder reply")?;
    if let Some(err) = reply.get("err") {
        bail!("{}", err["message"].as_str().unwrap_or("cannot attach"));
    }
    eprintln!("attached to {target}; Ctrl-\\ detaches (the session keeps running)\r");
    let raw = Raw::enter()?;
    let mut stdout = tokio::io::stdout();
    let to_screen = async {
        let mut buf = [0u8; 8192];
        loop {
            match from.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if stdout.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                    let _ = stdout.flush().await;
                }
            }
        }
    };
    let from_keyboard = async {
        let mut stdin = tokio::io::stdin();
        let mut buf = [0u8; 1024];
        loop {
            let n = match stdin.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if let Some(i) = buf[..n].iter().position(|b| *b == DETACH) {
                let _ = write.write_all(&buf[..i]).await;
                break;
            }
            if write.write_all(&buf[..n]).await.is_err() {
                break;
            }
        }
    };
    tokio::select! {
        _ = to_screen => {}
        _ = from_keyboard => {}
    }
    drop(raw);
    eprintln!("\r\ndetached from {target}");
    Ok(())
}
