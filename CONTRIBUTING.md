# Contributing to Task Journal

Thanks for your interest. Task Journal is a small Rust workspace; the
contribution loop is short by design.

## Before you start

- Read [README.md](README.md) for what the project is for.
- Read [CHANGELOG.md](CHANGELOG.md) for the current direction.
- Search [open issues](https://github.com/Digital-Threads/Task-Journal/issues)
  before filing a duplicate.

## Development setup

```bash
git clone https://github.com/Digital-Threads/Task-Journal
cd Task-Journal
cargo test --workspace
```

Minimum supported Rust version: see `rust-version` in [Cargo.toml](Cargo.toml).

## What I look for in a PR

1. **One thing per PR.** Bug fix or feature, not both. Refactors get their
   own PR. If you find a side issue while working, open a separate issue.
2. **A failing test before the fix.** For bugs, the test should reproduce
   the bug at HEAD (red) and pass with your change (green). For features,
   the test should describe the new behavior.
3. **Conventional commit prefix.** `fix:` / `feat:` / `chore:` / `docs:` /
   `perf:` / `refactor:` / `test:` / `ci:`. Add `!` for breaking changes.
4. **CI green.** That means `cargo fmt --all -- --check`,
   `cargo clippy --workspace --all-targets -- -D warnings`,
   `cargo test --workspace --all-targets`, `cargo doc --workspace --no-deps`
   with `RUSTDOCFLAGS=-D warnings`.
5. **CHANGELOG entry** if your change affects users (CLI flag, MCP tool,
   on-disk format, public API). One line under the relevant `## [Unreleased]`
   subsection (Added / Changed / Removed / Fixed / BREAKING).

## What I won't merge

- Cosmetic-only refactors of code that's been stable and tested.
- New abstractions without a second concrete user.
- Features that move the project away from "reasoning-chain memory for
  AI coding sessions" — please open an issue first to discuss scope.

## Reporting bugs

Use the bug template under [`.github/ISSUE_TEMPLATE/`](.github/ISSUE_TEMPLATE/).
The most useful bug reports include:

- The exact command you ran (or MCP tool call).
- The output you got vs. what you expected.
- `task-journal --version` and `rustc --version`.
- The contents of `task-journal doctor --json` if the bug looks like an
  installation/environment problem.

## License

By contributing you agree your work is licensed under the MIT License
(see [LICENSE](LICENSE)).
