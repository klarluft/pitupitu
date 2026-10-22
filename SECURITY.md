# Security

The box agent runs on your machine with your user's rights and holds a key that links the machine to your pitupitu account, so we take reports seriously and answer them quickly.

## Reporting a vulnerability

Please do not open a public issue for a security problem.

- Preferred: [report it privately on GitHub](https://github.com/klarluft/pitupitu/security/advisories/new) (Security → Report a vulnerability). You get a private thread with the maintainers.
- Or email **privacy@pitupitu.chat**.

Include what you found, how to reproduce it, and the version (`pitupitu --version`). You will hear back within three working days. We ask that you give us a reasonable time to fix the problem before you talk about it publicly, and we will credit you in the release notes if you like.

## Scope

In scope: everything in this repository (the `pitupitu` binary, the install script, the Homebrew formula, the release workflow) and the box link protocol as this agent implements it. Problems on the hosted side, pitupitu.chat and `api.pitupitu.chat`, are welcome at the same addresses.

Out of scope: the coding agents themselves (Claude Code, Codex and the others run as your own, unmodified CLIs) and [herdr](https://github.com/herdrdev/herdr), which has its own project; we will pass a report on if you prefer.

## What the agent does and does not do

The design limits what a compromised cloud could do on your machine; these are the properties worth testing:

- It only dials out (`wss://api.pitupitu.chat/v1/box`, TLS). Nothing listens on your machine.
- The cloud can only ask for the named, typed calls in [docs/box-link.md](docs/box-link.md): no shell, no arbitrary command, no file reads outside the listed ones.
- It never reads your agents' credentials or adds permission flags to them.
- It never updates itself or herdr without being asked, and checks the release's SHA-256 before replacing the binary.
- Logs carry metadata only, never prompts, agent output or file contents. Audio is never stored.

## Supported versions

Fixes go into a new release; the cloud accepts the current and the previous box protocol, so keeping `pitupitu update` within a release or two is enough. Older versions get no backported fixes.
