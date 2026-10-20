//! `pitupitu update` end to end against a stand-in release server: the
//! redirect that names the newest version, the archive and its checksum,
//! the atomic replacement of the binary. Nothing is restarted.

use axum::response::Redirect;
use axum::routing::get;
use axum::Router;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::process::Command;

const NAME: &str = if cfg!(target_os = "macos") {
    if cfg!(target_arch = "aarch64") {
        "pitupitu-aarch64-apple-darwin"
    } else {
        "pitupitu-x86_64-apple-darwin"
    }
} else if cfg!(target_arch = "aarch64") {
    "pitupitu-aarch64-unknown-linux-musl"
} else {
    "pitupitu-x86_64-unknown-linux-musl"
};

/// A release 9.9.9 whose binary is a script that says so.
fn release(dir: &std::path::Path) -> (Vec<u8>, String) {
    let pkg = dir.join(NAME);
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(pkg.join("pitupitu"), "#!/bin/sh\necho fake 9.9.9\n").unwrap();
    let tgz = dir.join(format!("{NAME}.tar.gz"));
    let ok = Command::new("tar")
        .arg("czf")
        .arg(&tgz)
        .arg("-C")
        .arg(dir)
        .arg(NAME)
        .status()
        .unwrap()
        .success();
    assert!(ok);
    let bytes = std::fs::read(&tgz).unwrap();
    let sum: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    (bytes, format!("{sum}  {NAME}.tar.gz\n"))
}

async fn serve(archive: Vec<u8>, sum: String, bad_sum: bool) -> String {
    let sum = if bad_sum {
        format!("{}0{}", &sum[..10], &sum[11..])
    } else {
        sum
    };
    let app = Router::new()
        .route(
            "/latest",
            get(|| async { Redirect::temporary("/tag/v9.9.9") }),
        )
        .route("/tag/v9.9.9", get(|| async { "release page" }))
        .route(
            &format!("/download/v9.9.9/{NAME}.tar.gz"),
            get(move || async move { archive }),
        )
        .route(
            &format!("/download/v9.9.9/{NAME}.tar.gz.sha256"),
            get(move || async move { sum }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

/// The binary under test, copied where the test may replace it, run with
/// nothing of the person's reachable: no herdr. Run
/// asynchronously so the stand-in server on this runtime keeps serving.
async fn run(exe: &PathBuf, url: &str, args: &[&str]) -> (bool, String) {
    let mut tries = 0;
    let out = loop {
        let spawned = tokio::process::Command::new(exe)
            .args(args)
            .env("PITUPITU_RELEASES_URL", url)
            .env("HERDR_BIN", "/nonexistent/herdr")
            .stdin(std::process::Stdio::null())
            .output()
            .await;
        match spawned {
            // "Text file busy": the other test's fork still holds the copy
            // we just wrote open for a moment.
            Err(e) if e.raw_os_error() == Some(26) && tries < 20 => {
                tries += 1;
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            other => break other.unwrap(),
        }
    };
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

#[tokio::test]
async fn the_update_is_shown_asked_for_verified_and_installed_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let (archive, sum) = release(&dir.path().join("release"));
    let url = serve(archive, sum, false).await;
    let exe = dir.path().join("pitupitu");
    std::fs::copy(env!("CARGO_BIN_EXE_pitupitu"), &exe).unwrap();

    // Only looking.
    let (ok, text) = run(&exe, &url, &["update", "--check"]).await;
    assert!(ok, "{text}");
    assert!(text.contains("9.9.9"), "{text}");
    assert!(!text.contains("installed"), "{text}");
    let (ok, text) = run(&exe, &url, &["--json", "update", "--check"]).await;
    assert!(ok, "{text}");
    let v: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
    assert_eq!(v["latest"], "9.9.9");
    assert_eq!(v["update_available"], true);

    // Without a terminal and without --yes: nothing happens.
    let (ok, text) = run(&exe, &url, &["update", "--no-restart"]).await;
    assert!(!ok, "{text}");
    assert!(text.contains("--yes"), "{text}");
    let (ok, text) = run(&exe, &url, &["--version"]).await;
    assert!(ok && !text.contains("9.9.9"), "{text}");

    // Said yes: what is running is listed, then the binary is replaced.
    let (ok, text) = run(&exe, &url, &["update", "--yes", "--no-restart"]).await;
    assert!(ok, "{text}");
    assert!(text.contains("keep running"), "{text}");
    assert!(text.contains("installed pitupitu 9.9.9"), "{text}");
    let out = Command::new(&exe).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "fake 9.9.9");
}

#[tokio::test]
async fn a_bad_checksum_installs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (archive, sum) = release(&dir.path().join("release"));
    let url = serve(archive, sum, true).await;
    let exe = dir.path().join("pitupitu");
    std::fs::copy(env!("CARGO_BIN_EXE_pitupitu"), &exe).unwrap();
    let (ok, text) = run(&exe, &url, &["update", "--yes", "--no-restart"]).await;
    assert!(!ok, "{text}");
    assert!(text.contains("checksum"), "{text}");
    let (ok, text) = run(&exe, &url, &["--version"]).await;
    assert!(ok && text.contains(env!("CARGO_PKG_VERSION")), "{text}");
}
