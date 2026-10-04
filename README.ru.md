# docsbase — локальная память документации для кодовых агентов

[![CI](https://github.com/punkhomov/docsbase-memory-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/punkhomov/docsbase-memory-mcp/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/punkhomov/docsbase-memory-mcp?include_prereleases)](https://github.com/punkhomov/docsbase-memory-mcp/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)
[![MSRV 1.88](https://img.shields.io/badge/MSRV-1.88-blue.svg)](rust-toolchain.toml)

> **Альфа:** текущая версия [`0.1.0-alpha.1`](CHANGELOG.md). До `0.1.0`
> возможны ломающие изменения. Git-тег `v<version>` всегда равен
> `version` в `Cargo.toml`.
>
> Читать на другом языке: **Русский** · [English](README.md)

Один бинарь `docsbase`, три роли: **CLI**, **stdio MCP-сервер** и
**персональный демон**. Демон владеет реестром проектов, per-project
полнотекстовыми индексами, вотчерами и sync-джобами. Единственный источник
истины — `.md`-файлы на диске; MCP-поверхность — read-only поверх индекса.
В рантайме ноль сетевых вызовов: ни телеметрии, ни облака, ни Docker.

Поддерживаемые платформы: **Linux x86_64** (статический бинарь) и
**Windows x64** (`.exe`). macOS вне scope v1.

## Быстрый старт (5 минут)

```sh
# 1. Установка из ассета GitHub Releases (варианты — в docs/INSTALL.md)
curl -fsSL https://raw.githubusercontent.com/punkhomov/docsbase-memory-mcp/main/install.sh | sh
# Windows (PowerShell):
# irm https://raw.githubusercontent.com/punkhomov/docsbase-memory-mcp/main/install.ps1 | iex

# 2. Регистрация бинаря + манифеста
docsbase install

# 3. Индексация репозитория с доками (по умолчанию — текущий каталог)
cd /path/to/your/docs
docsbase index

# 4. Поиск
docsbase search "admission lease" --limit 10

# 5. Переиндексация по требованию
docsbase sync
docsbase status
```

Полное прохождение: [`docs/QUICKSTART.md`](docs/QUICKSTART.md) (EN).

## Установка

| Способ | Команда | Примечание |
|---|---|---|
| Linux-скрипт | `curl -fsSL …/install.sh \| sh` | Качает ассет релиза, проверяет sha256, вызывает `docsbase install` |
| Windows-скрипт | `irm …/install.ps1 \| iex` | То же для `.exe`-ассета |
| Cargo | `cargo install --locked docsbase --version 0.1.0-alpha.1` | Сборка из исходников, MSRV 1.88 |
| Вручную | Скачать ассет из [Releases](https://github.com/punkhomov/docsbase-memory-mcp/releases), `sha256sum -c`, затем `docsbase install` | Проверяйте SLSA-provenance, см. [`docs/INSTALL.md`](docs/INSTALL.md) |

Обновление: `docsbase update` (проверяет последний релиз, верифицирует и
заменяет бинарь). Зафиксировать версию: `docsbase update --version v0.1.0-alpha.1`.

Каждый релиз содержит: файлы `sha256sum`, SBOM и SLSA-provenance сборки
(Sigstore). Проверка: `gh attestation verify`.

## CLI кратко

```text
docsbase install            # установка/обновление бинаря + манифеста
docsbase update             # скачать последний релиз, проверить и заменить бинарь
docsbase index [PATH]       # регистрация и индексация проекта (по умолч. cwd)
docsbase search QUERY [--limit N]
docsbase list               # документы текущего проекта в индексе
docsbase sync [PATH]        # переиндексация (джоба демона, иначе напрямую)
docsbase config             # эффективный конфиг для текущего каталога
docsbase status             # статус реестра
docsbase mcp                # stdio MCP-сервер (для агентов, не вручную)
docsbase daemon stop        # остановить демона
docsbase uninstall [--yes]  # без --yes только показывает; с --yes удаляет
```

Детали: [`docs/CONFIG.md`](docs/CONFIG.md), `docsbase --help`.

## Настройка MCP

`docsbase mcp` — stdio-сервер. Регистрируется один раз на клиент агента:

```json
{
  "mcpServers": {
    "docsbase": { "command": "docsbase", "args": ["mcp"] }
  }
}
```

Инструменты: `search_docs`, `get_doc`, `read_neighbors`, `list_docs`,
`list_projects`, `index_project`, `sync_start`, `sync_status`.
Детали и таймауты: [`docs/MCP.md`](docs/MCP.md) (EN).

## Как устроено

```text
агенты (stdio JSON-RPC) -> docsbase mcp (frontend) -> local socket/pipe -> daemon
CLI ----------------------------------------------------------> daemon / snapshot
daemon -> registry.db (SQLite WAL) + projects/<id>/tantivy + вотчер на проект
```

Весь ОС-зависимый код изолирован за `src/platform/` (Unix-сокет vs Windows
named pipe). Публичные контракты (MCP-инструменты, CLI, формат конфига,
сокет-протокол) версионируются; ломающие изменения — только мажорная версия.
Подробнее: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) (EN).

## Конфигурация

Приоритет: **CLI > проект > глобал > defaults**.

- Глобально: `~/.config/docsbase-memory-mcp/config.toml`
- Проект: `.docsbase.toml` + `.docsbaseignore` в корне проекта
- Переменные: `DOCSBASE_CACHE_DIR`, `DOCSBASE_DATA_DIR`, `DOCSBASE_CONFIG_DIR`
- Дефолты: `max_file_size = 1 MiB`, `max_docs_per_project = 20000`,
  `auto_index = false`, `hybrid = false`

Пример и все ключи: [`docs/CONFIG.md`](docs/CONFIG.md) (EN).

## Разработка

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo test --locked --release   # включая перф-бюджеты (однопоточно)
```

Тулчейн: `rust-toolchain.toml` пинит nightly для локальной разработки; CI
проверяет MSRV 1.88 явно. `Cargo.lock` коммитится; новые крейты — только
через ADR и при `min-publish-age = 14 days`.

Ветки: GitHub Flow (`main` + PR + теги `v*`). Conventional Commits. Релизы
только по тегам, с provenance — см.
[`docs/specs/constitution.md`](docs/specs/constitution.md).

## Безопасность

По дизайну — только локально: сокет/pipe на localhost, `0600`/`0700` на Unix
(на Windows — best-effort ACL), файловые операции только внутри
зарегистрированных roots, удаление только по валидированному манифесту.
Политика supply chain (модель угроз, карантин, что покрывает Dependabot, а что нет):
[`docs/supply-chain.md`](docs/supply-chain.md) (EN).
Репорты — по [`SECURITY.md`](SECURITY.md).

## Лицензия

MIT — см. [LICENSE](LICENSE).
