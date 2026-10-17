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
- When testing against a live herdr, create your own workspace or tab and close it afterwards; don't touch panes you didn't create.
- Logs carry metadata only: never prompts, agent output or file contents.

## Developer Certificate of Origin

Contributions are accepted under the [Developer Certificate of Origin](https://developercertificate.org/) (DCO 1.1). Sign off each commit to certify that you wrote it or otherwise have the right to submit it under this project's licence:

```sh
git commit -s
```

which adds a line like `Signed-off-by: Your Name <you@example.com>` to the commit message.

## Licence

By contributing you agree that your contributions are licensed under the [Apache License 2.0](LICENSE).
