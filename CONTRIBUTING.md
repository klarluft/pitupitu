# Contributing

Thanks for helping. Issues and pull requests are welcome.

## Before you open a pull request

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

- `crates/herdr` is the only code that runs `herdr`. Use `herdr --skill` and `herdr api schema` for exact commands and payloads.
- The box link is a contract with pitupitu cloud ([docs/box-link.md](docs/box-link.md)). A new call needs a protocol bump; never widen what the cloud can do on the box beyond named, typed calls.
- When testing against a live herdr by hand, use a named session (`herdr --session <name>`), create your own workspaces and tabs, and stop and delete the session afterwards; don't touch panes you didn't create.
- Tests that need a live herdr start a server in a throwaway named session and stop and delete it afterwards, so they never touch your own herdr; they skip when `herdr` is not on PATH. `PITUPITU_HERDR_SESSION=<name>` points the box agent itself at a named session the same way (for trying it out, not for use). `PITUPITU_LIVE_HERDR=1 cargo test --test conformance` also runs the session scenario (`docs/sessions.md`) with the signed-in Claude Code on your machine, in a temporary folder, and should pass before a release.
- Logs carry metadata only: never prompts, agent output or file contents.

## Releasing

Maintainers release by pushing a tag:

1. Bump `version` in the root `Cargo.toml`, run `cargo build` so `Cargo.lock` follows, and merge that to `main`.
2. Tag the merged commit and push the tag:

   ```sh
   git tag -a v0.5.0 -m "pitupitu 0.5.0"
   git push origin v0.5.0
   ```

3. `.github/workflows/release.yml` builds four targets (Linux x86_64 and arm64 with musl, macOS x86_64 and arm64), packages each with a `.sha256`, fills in the Homebrew formula from those checksums (`packaging/homebrew/`), and publishes a GitHub release with the tarballs, `pitupitu.rb` and generated notes. `install.sh` downloads from the latest release.
4. `klarluft/homebrew-tap` copies `pitupitu.rb` from the latest release within a few hours. To pick it up now: `gh workflow run bump-pitupitu.yml --repo klarluft/homebrew-tap`.

The cloud accepts the current and the previous box protocol, so a release that bumps the protocol needs the cloud side live first.

## Developer Certificate of Origin

Contributions are accepted under the [Developer Certificate of Origin](https://developercertificate.org/) (DCO 1.1). Sign off each commit to certify that you wrote it or otherwise have the right to submit it under this project's licence:

```sh
git commit -s
```

which adds a line like `Signed-off-by: Your Name <you@example.com>` to the commit message.

## Licence

By contributing you agree that your contributions are licensed under the [Apache License 2.0](LICENSE).
