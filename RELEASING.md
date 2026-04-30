# Releasing

Чеклист для каждого релиза `task-journal` v0.X.Y.

## Pre-release (один раз для проекта)

1. **GitHub repo создан**, origin подключён.
2. **crates.io аккаунт** заведён → Settings → API Tokens → создать токен `task-journal-publish` с scope `publish-new`.
3. **GitHub Secrets** (Settings → Secrets and variables → Actions → New repository secret):
   - `CRATES_IO_TOKEN` = токен из шага 2.
4. **Crate names проверены** — `cargo search task-journal-core task-journal-cli task-journal-mcp` показывает что свободны. Если заняты — поменять в Cargo.toml каждого crate (`name = "..."`).

## Per-release flow

### 1. Подготовка

```bash
# Убедись что main green
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check

# Bump version в Cargo.toml workspace (раз — все crates наследуют через version.workspace = true)
# Edit Cargo.toml: version = "0.1.1" (или 0.2.0, 1.0.0, etc.)
git add Cargo.toml
git commit -m "chore: bump version to v0.1.1"
git push
```

### 2. Tag + push

```bash
git tag v0.1.1
git push origin v0.1.1
```

Это автоматически триггерит:
- **`.github/workflows/release.yml`** — собирает pre-built бинарники под Linux/macOS-x86_64/macOS-arm64/Windows и публикует GitHub Release с прикреплёнными `tar.gz`/`.zip` + `checksums.txt`.
- **`.github/workflows/publish.yml`** — публикует все 3 crate'а на crates.io (нужен `CRATES_IO_TOKEN` secret).

### 3. Если автопубликация не нужна — руками

```bash
cargo login   # paste crates.io token
cargo publish -p task-journal-core
sleep 30
cargo publish -p task-journal-cli
cargo publish -p task-journal-mcp
```

### 4. Проверь что вышло

- GitHub Releases страница: https://github.com/shahinyanm/claude-memory/releases — должен быть `v0.1.1` с 4 артефактами.
- crates.io: https://crates.io/crates/task-journal-core (и cli, mcp) — должна появиться версия v0.1.1.

### 5. Обновить README

Если первая публикация — добавить badge'и:

```markdown
[![CI](https://github.com/shahinyanm/claude-memory/workflows/CI/badge.svg)](https://github.com/shahinyanm/claude-memory/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/task-journal-cli.svg)](https://crates.io/crates/task-journal-cli)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
```

## Post-release

Для каждой следующей версии:
- Обнови `version` в `Cargo.toml` workspace
- `git tag vX.Y.Z && git push origin vX.Y.Z`
- CI делает остальное

## Troubleshooting

| Симптом | Что проверить |
|---------|---------------|
| `cargo publish` падает с "name already taken" | Имя crate занято на crates.io. Поменяй `name = "..."` в Cargo.toml на другое. |
| Release workflow не запустился | Тэг должен начинаться с `v` (например `v0.1.0`, не `0.1.0` или `release-0.1.0`). |
| `cargo publish -p task-journal-cli` падает с "task-journal-core not found" | Не подождал `sleep 30` после публикации `task-journal-core` — повтори через минуту. |
| Pre-built binary не работает на macOS — "killed" | Apple Silicon vs Intel — скачай правильный target (`aarch64-apple-darwin` для M1/M2/M3, `x86_64-apple-darwin` для Intel). |
