# Constitution — docsbase-memory-mcp

> Принципы проекта. Действуют для всех спеков. Меняются осознанно, а не под задачу.
> Версия: 1.1.0. Дата: 2026-10-04.
> Изменение 1.1.0: платформа v1 — Linux x86_64 + Windows x64 (ADR-10 реализован,
> T38–T43); macOS out of scope. Версионирование — SemVer с `alpha`-пререлизами
> (`v0.1.0-alpha.1`); тег обязан совпадать с `version` в `Cargo.toml`.

## Платформа и язык

- Rust **nightly**, зафиксирован в `rust-toolchain.toml` (`channel = "nightly"`,
  components: rustfmt, clippy). Проверенный билд: `cargo 1.101.0-nightly (3d7cf6e93
  2026-09-25)`, `rustc 1.101.0-nightly (d080e7dff 2026-09-27)`. Edition 2024.
- Сборка: `cargo build --release`.

## Supply-chain

- **`min-publish-age` = 14 дней** (`.cargo/config.toml`):
  `[registry] global-min-publish-age = "14 days"` и
  `[resolver] incompatible-publish-age = "deny"`. Фича стабилизирована в nightly
  (проверено эмпирически 2026-09-28: `[unstable]` не требуется, слишком свежие версии
  отклоняются с `too new (published N days ago, minimum age 14 days)`).
- Точечный обход только через
  `CARGO_RESOLVER_INCOMPATIBLE_PUBLISH_AGE=allow cargo update <crate> --precise <ver>`
  с обоснованием в задаче; молчаливый обход запрещён.
- Релизы — только через GitHub Releases по тегу `v*` (GitHub Flow):
  артефакты Linux + Windows, `sha256sum`, SBOM и SLSA-provenance
  (Sigstore, `actions/attest-build-provenance`). Только checksums без
  provenance — недостаточно для релиза.
- Enforcement `min-publish-age` сегодня — только локально под nightly
  (solo-режим: весь lock генерируется там; Dependabot сдержан `cooldown` +
  выключенными version-апдейтами). CI на stable ключ игнорирует — см.
  `docs/supply-chain.md`; нативный CI-гейт — после стабилизации механизма
  (~cargo 1.100).
- `Cargo.lock` коммитится; CI собирает с `--locked`.
- v1 — Linux x86_64 и Windows x64 (ADR-10 реализован: daemon + CLI + MCP,
  зелёные тесты в CI на обеих ОС). macOS out of scope до отдельного решения.
- Версионирование — SemVer. До стабилизации — пререлизы `0.1.0-alpha.N`;
  Git-тег `v<version>` обязан совпадать с `version` в `Cargo.toml` (проверяется в CI).
  Ломающее изменение публичных контрактов — только мажорная версия.
- Один бинарь `docsbase` на платформу: Linux x86_64 — статический
  (`+crt-static`, без динамических зависимостей); Windows x64 — `.exe`
  (статик не требуется, SDDL-ACL hardening — backlog после T43).
  Никаких внешних сервисов в рантайме (никаких
  Docker/Postgres/Python).
- Сеть не используется: после установки ни одного сетевого запроса, включая телеметрию.

## Качество кода

- `cargo fmt` обязателен; `cargo clippy --all-targets -- -D warnings` — ноль warning.
- Lints: `unsafe_code = "warn"`, clippy `all` + `pedantic` (точечные allow — с комментарием).
- `unwrap`/`expect`/`panic!` запрещены в библиотечном коде. В `main`/CLI допустимы только
  для «непоправимых» стартовых ошибок; в тестах — свободно.
- Ошибки: `thiserror` в библиотеках, `anyhow` только на границе бинаря/CLI. Ошибки не
  теряют контекст при пересечении границ модулей.
- `unsafe` — только с `SAFETY:`-комментарием и тестом; цель — ноль `unsafe` в v1.

## Тестирование

- На каждый `FR-*` — минимум один автоматизированный тест.
- TDD при исполнении: сначала падающий тест, потом минимальный код, потом весь suite.
- Интеграционные тесты обязательны для: daemon lifecycle, admission, watcher,
  поиска (golden-запросы), MCP tools.
- «Готово» только при зелёных `cargo test` + `cargo clippy` в этом же прогоне.

## Зависимости

- Новые зависимости берутся только из таблицы «Dependencies + rationale» в `design.md`;
  крейт вне этого списка — только через ADR.
- Каждый добавленный крейт обосновывается в шапке задачи (`New crates:`).
- Версии проверяются фактически (`cargo add --dry-run` / lockfile), не по памяти.
- Никаких embedding/LLM API и облачных сервисов.
- Смена базового стека — только через ADR.

## Данные и совместимость

- Один canonical cache root; переключение только при остановленном демоне.
- Схема БД версионируется; несовместимое изменение on-disk формата требует миграции или
  документированной пересборки индекса.
- Один writer на индекс; запись crash-safe (WAL + lock recovery).
- Индексы — производные данные: их допустимо пересобрать из исходных `.md`.

## Контракты и границы

- Файловые операции только внутри зарегистрированных roots; выход за root (включая
  symlink) отклоняется.
- MCP — read-only. Единственный источник истины — `.md` файлы на диске.
- Публичные контракты (MCP tools, CLI, формат конфига, socket-протокол) версионируются;
  ломающее изменение — только мажорная версия.

## Язык документации

- Спеки и ADR — русский; идентификаторы, код, комментарии, commit messages — английский.
