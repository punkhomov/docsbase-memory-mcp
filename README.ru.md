# docsbase — локальная память документации для кодовых агентов

[![CI](https://github.com/punkhomov/docsbase-memory-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/punkhomov/docsbase-memory-mcp/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/punkhomov/docsbase-memory-mcp?include_prereleases)](https://github.com/punkhomov/docsbase-memory-mcp/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)
[![MSRV 1.88](https://img.shields.io/badge/MSRV-1.88-blue.svg)](rust-toolchain.toml)

> **Альфа:** текущая версия [`0.1.0-alpha.4`](CHANGELOG.md). До `0.1.0`
> возможны ломающие изменения. Git-тег `v<version>` всегда равен
> `version` в `Cargo.toml`.
>
> Читать на другом языке: **Русский** · [English](README.md)

Ядро — обычная Rust-библиотека (`docsbase_memory`); бинарь `docsbase` — её
первый потребитель с тремя ролями: **CLI**, **stdio MCP-сервер** и
**персональный демон**. Встраивание библиотеки в приложения и пайплайны
(cargo-зависимость) — явное долгосрочное направление; стабильность API до 1.0
не обещается. Демон владеет реестром проектов, per-project
полнотекстовыми индексами, вотчерами и sync-джобами. Единственный источник
истины — `.md`-файлы на диске; MCP-поверхность — read-only поверх индекса.
В рантайме по умолчанию ноль сетевых вызовов: ни телеметрии, ни облака, ни
Docker. Опциональные hybrid-стадии поиска (embeddings + rerank через
OpenRouter, плюс opt-in RRF-fusion по кэшу векторов — `ranking = "rrf"`)
включаются явно per-project/per-request, поставляются за feature-флагом
`hybrid-remote` и требуют `OPENROUTER_API_KEY`; дефолтная сборка остаётся
полностью офлайн и не делает молчаливого fallback
(см. [`docs/CONFIG.md`](docs/CONFIG.md)). Дефолтные пины моделей —
бесплатные маршруты OpenRouter: могут учиться на промптах и меняются
ежемесячно; платные пины — через `docsbase models use`.

Поддерживаемые платформы: **Linux x86_64** (статический бинарь),
**Windows x64** (`.exe`) и **macOS 12+ на Apple Silicon**. Intel-маки вне
scope (`x86_64-apple-darwin` — Tier 2 у upstream, Intel-железо EOL).

## Быстрый старт (5 минут)

```sh
# 1. Установка бинаря. Скрипт проверяет релиз и сам регистрирует бинарь
#    в docsbase. Альфа-период: все релизы — пререлизы, включаем канал
#    явно; после 0.1.0 по умолчанию стабильный канал.
curl -fsSL https://raw.githubusercontent.com/punkhomov/docsbase-memory-mcp/main/install.sh | DOCSBASE_CHANNEL=prerelease sh

# 2. Индексация репозитория с доками (по умолчанию — текущий каталог)
cd /path/to/your/docs
docsbase index

# 3. Поиск
docsbase search "admission lease" --limit 10

# 4. Переиндексация по требованию
docsbase sync
docsbase status
```

Windows (PowerShell):

```powershell
$env:DOCSBASE_CHANNEL = "prerelease"; irm https://raw.githubusercontent.com/punkhomov/docsbase-memory-mcp/main/install.ps1 | iex
```

Сборка из исходников или ручная установка? Выполните `docsbase install` один
раз, чтобы зарегистрировать бинарь — скрипты выше делают это сами.

Полное прохождение: [`docs/QUICKSTART.md`](docs/QUICKSTART.md) (EN).

## Установка

Поддерживаются: **Linux x86_64**, **Windows x64** и **macOS (Apple
Silicon)**. Подробности, проверка и ручные варианты:
[`docs/INSTALL.md`](docs/INSTALL.md) (EN).

### Linux x86_64

```sh
curl -fsSL https://raw.githubusercontent.com/punkhomov/docsbase-memory-mcp/main/install.sh | DOCSBASE_CHANNEL=prerelease sh
```

Ставит бинарь в `~/.local/bin` (переопределяется `DOCSBASE_BIN_DIR`),
проверяет `sha256` и регистрирует бинарь (`docsbase install`). Зафиксировать
версию: `DOCSBASE_VERSION=v0.1.0-alpha.4`.

### Windows x64 (PowerShell)

```powershell
$env:DOCSBASE_CHANNEL = "prerelease"; irm https://raw.githubusercontent.com/punkhomov/docsbase-memory-mcp/main/install.ps1 | iex
```

Ставит в `%USERPROFILE%\.local\bin` (добавляется в пользовательский `PATH`),
проверяет `sha256` и регистрирует бинарь. Тот же `DOCSBASE_VERSION`.

### macOS (Apple Silicon)

```sh
curl -fsSL https://raw.githubusercontent.com/punkhomov/docsbase-memory-mcp/main/install.sh | DOCSBASE_CHANNEL=prerelease sh
```

Ставит в `~/.local/bin`, проверяет `sha256` (через стоковый `shasum`) и
регистрирует бинарь. Бинарь ad-hoc подписан; `curl` не ставит карантин, так
что установка из терминала работает без запросов Gatekeeper (нотаризация для
этого пути не нужна).

### Сборка из исходников (любая поддерживаемая платформа)

```sh
cargo install --locked docsbase --version 0.1.0-alpha.4
docsbase install   # зарегистрировать бинарь в демоне
```

Требуется Rust 1.88+ (MSRV). Для локальной разработки в `rust-toolchain.toml`
зафиксирован nightly; CI проверяет MSRV-тулчейн.

### Вручную

Скачайте ассет и его `.sha256` из
[Releases](https://github.com/punkhomov/docsbase-memory-mcp/releases),
проверьте (`sha256sum -c` на Linux, `shasum -a 256 -c` на macOS,
`Get-FileHash` на Windows), при желании
проверьте SLSA-provenance через `gh attestation verify`, положите бинарь в
`PATH` и выполните `docsbase install`.

### Обновление и проверка

`docsbase update` проверяет последний релиз, верифицирует и заменяет бинарь
(зафиксировать версию: `docsbase update --version v0.1.0-alpha.4`). Каждый
релиз содержит файлы `sha256sum`, SBOM и SLSA-provenance сборки (Sigstore).

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
`list_projects`, `index_project`, `sync_start`, `sync_status`, `status`.
Детали и таймауты: [`docs/MCP.md`](docs/MCP.md) (EN).

## Как устроено

```text
агенты (stdio JSON-RPC) -> docsbase mcp (frontend) -> local socket/pipe -> daemon
CLI ----------------------------------------------------------> daemon / snapshot
daemon -> registry.db (SQLite WAL) + projects/<id>/tantivy + вотчер на проект
```

Весь ОС-зависимый код изолирован за `src/platform/` (Unix-сокет на
Linux/macOS vs Windows named pipe). Публичные контракты (MCP-инструменты, CLI, формат конфига,
сокет-протокол) версионируются; ломающие изменения — только мажорная версия.
Подробнее: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) (EN).

## Мультиязычный поиск

Поиск офлайн и без словарей: никаких новых крейтов и сети. Токенайзер
учитывает скрипт и версионируется — смена пайплайна бампает
`TOKENIZER_VERSION`, и индекс пересобирается из SQLite автоматически.

| Скрипт | Нормализация | Анализатор | Пример |
|---|---|---|---|
| Кириллица | — | Snowball Russian (standalone-слова) | `замены` находит `замена` |
| Латиница | fullwidth → ASCII, `İ` → `I` | Snowball English | `storing` находит `store` |
| Арабский | harakat/tatweel удаляются, `أإآ`→`ا`, `ة`→`ه`, `ى`→`ي` | Snowball Arabic | `مَطَار` находит `مطار` |
| CJK | — | биграммы по всем символам, последовательные позиции | `東京` внутри `東京国際空港` |
| Все | `ё`→`е` (с сохранением регистра), width folding | — | `ёлка` находит `елка` |
| Идентификаторы | не стеммятся (`_`, CamelCase, ALL_CAPS, цифры) | точные мульти-токены | `assessment_plan_id`, `MAX_FRAME_BYTES` |

Замеры на SQ18 (directional; замороженная копия хранится как
[`tests/fixtures/search-bench/legacy-baseline.json`](tests/fixtures/search-bench/legacy-baseline.json)
после вывода search-quality harness):
все шесть golden-классов hit_rate/ndcg 1.0; p95 поиска ≈ 0.6 мс (латиница) и
≈ 5 мс (CJK) на 50k чанков; CJK-индекс ≈ 0.8× размера текста корпуса.

Примечания: BM25-скоры — внутренние для движка и намеренно не снапшотятся
(дрейфуют между версиями tantivy); fence/table-блоки атомарны (могут
превышать cap); фразовый матч никогда не срабатывает внутри идентификаторов
(`"plan id"` ≠ `assessment_plan_id`).

## Конфигурация

Приоритет: **CLI > проект > глобал > defaults**.

- Глобально: `~/.config/docsbase-memory-mcp/config.toml`
  (`~/Library/Application Support/docsbase-memory-mcp/config.toml` на macOS)
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
