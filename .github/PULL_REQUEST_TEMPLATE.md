## Summary

<!-- One paragraph: what changes and why. -->

## Type of change

- [ ] Bug fix (non-breaking)
- [ ] Feature (non-breaking)
- [ ] Breaking change (CLI flag, MCP tool shape, on-disk format)
- [ ] Refactor / chore / docs / CI only

## Test plan

- [ ] Failing test added before the fix (bug) or describing the new
  behavior (feature)
- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace --all-targets`
- [ ] `cargo doc --workspace --no-deps` with `RUSTDOCFLAGS=-D warnings`

## CHANGELOG

- [ ] Added an entry under `## [Unreleased]` (Added / Changed / Removed
  / Fixed / BREAKING) — or this PR doesn't affect users.

## Related issues

<!-- "Closes #123" / "Refs #456" -->
