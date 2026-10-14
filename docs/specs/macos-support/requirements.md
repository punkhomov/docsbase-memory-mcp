# macOS support — requirements

Статус: draft (спека цикла M1–M4). Контекст-исследование: агенты + внешние
источники (см. design §1 «Verified facts»).

## 1. Goal

Поддержать macOS (Apple Silicon) на уровне Windows: сборка из исходников,
зелёный функциональный сьют в CI, релизный артефакт, installer/update, доки.

## 2. Scope

**In scope (v1 macOS):**
- aarch64 macOS (CI: `macos-latest` = macOS 26 arm64; локально: macOS 12+).
- Платформенный шов: liveness, счётчики, системные каталоги, guard длины сокета.
- CI-джоба `macos` (функциональный сьют — паритет с Windows).
- Релиз: `docsbase-<ver>-macos-aarch64.tar.gz` + sha256 + attest + ad-hoc подпись.
- `install.sh` (Darwin), `docsbase update` (asset mapping), доки/конституция.

**Out of scope (v1):**
- x86_64 macOS и universal-бинарь (Intel EOL; `x86_64-apple-darwin` — Tier 2).
- Нотаризация (нужен Apple Developer Program; триггер — браузерная дистрибуция).
- Homebrew tap, .pkg/.dmg, macOS < 12, Rosetta-специфика.
- `offline`/`perf_budget`/`artifact` сьюты на macOS (как и на Windows — Linux-only,
  gap документируется).

## 3. Verified facts (внешние источники)

- `sun_path[104]` на macOS (`man 4 unix`); ядро умеет до 253 с pointer-трюками,
  std/tokio ограничены 104.
- `curl` не ставит `com.apple.quarantine` (MITRE T1553.001) → unsigned/ad-hoc
  бинарь запускается из терминала; Gatekeeper не проверяет.
- arm64 требует валидную подпись; линкер ставит ad-hoc; после модификации
  (strip/lipo) подпись переставляем `codesign -s - --force` + gate `codesign -v`.
- `macos-latest` = macOS 26 arm64 (миграция завершена 2026-07-22); `macos-14`
  ретирится 2026-11-02; `macos-15-intel` — до августа 2027.
- `x86_64-apple-darwin` — Tier 2 с Rust 1.90 (RFC 3841).
- notify 8.2.0: FSEvents `latency = 0.0` + `NoDefer` (доставка немедленная).
- `libc 0.2.189` уже в графе (через `fsevent-sys`); `proc_pidinfo`/
  `PROC_PIDTBSDINFO`/`proc_bsdinfo`/`PROC_PIDLISTFDS`/`proc_fdinfo` — есть.

## 4. Functional requirements

- **FR-1** (must) Шов: macOS использует unix-бэкенд; все OS-вызовы остаются в
  `src/platform/` (граница тестируется `platform_boundary`); новых модулей нет.
- **FR-2** (must) Liveness: `process_alive` на macOS через
  `proc_pidinfo(PROC_PIDTBSDINFO)`; зомби (`SZOMB`) = мёртв — паритет с Linux
  `state != "Z"`. `stop_daemon`, janitor (`prune_dead`) и ожидания смерти
  процесса работают как на Linux.
- **FR-3** (must) Счётчики: `fd_count` на macOS через `PROC_PIDLISTFDS`;
  `thread_count` = 0 (документировано, паритет с Windows).
- **FR-4** (must) Системные каталоги: macOS-список блокировки корней проектов:
  SUBTREES `/System`, `/Library`, `/Applications`, `/private/etc`;
  EQUALS `/Users`, `/Volumes`, `/private/var`, `/private/tmp` (канонические
  пути; temp-каталоги тестов остаются валидными корнями).
- **FR-5** (should) Case handling: сравнение путей регистронезависимо (default
  APFS), как Windows; ограничение для case-sensitive томов документируется.
- **FR-6** (must) Socket guard: bind/connect с путём длиннее лимита
  (`sun_path` − NUL; macOS 104, Linux 108) возвращает понятную ошибку
  (`InvalidInput`) вместо сырого EINVAL/ENAMETOOLONG.
- **FR-7** (must) CI: джоба `macos` (macos-latest, MSRV 1.88): fmt, clippy
  `-D warnings`, `cargo test --locked` — паритет с Windows. Изначально
  advisory; перевод в required — отдельным решением (branch protection).
- **FR-8** (must) Release: артефакт `docsbase-<ver>-macos-aarch64.tar.gz` +
  `.sha256` + SLSA attest; ad-hoc подпись (`codesign -s - --force`) и gate
  `codesign -v` в пайплайне; `publish.needs` включает macOS.
- **FR-9** (must) Установка/обновление: `install.sh` принимает `Darwin` +
  `arm64→aarch64`, выбирает macOS-ассет; `docsbase update` умеет macOS и
  `shasum -a 256` (стоковый macOS без GNU `sha256sum`).
- **FR-10** (must) Тесты: Linux-only гейты для `/proc`-специфики; macOS-ветка
  `FRESHNESS_BUDGET` (5 с); per-OS список системных каталогов в тестах;
  smoke-тест релизного артефакта на macOS-раннере (запуск `docsbase --version`
  + `index/search` на фикстуре).
- **FR-11** (must) Доки: README(.ru), INSTALL, QUICKSTART, ARCHITECTURE,
  SECURITY, CHANGELOG; delta-маркер конституции (артефакты); AGENTS
  (required-checks при переводе); issue-template.

## 5. Non-functional requirements

- **NFR-1** MSRV 1.88 остаётся; сборка на macOS с MSRV тулчейном.
- **NFR-2** Новых крейтов нет: `libc` добавляется как macOS-target-dep, но уже
  присутствует в графе транзитивно (ADR-M1).
- **NFR-3** `unsafe` только внутри `src/platform/` с SAFETY-комментариями;
  zero-unsafe вне шва сохраняется (`unsafe_code = warn`).
- **NFR-4** Поведение Linux/Windows не меняется (все изменения — под
  `cfg(target_os = "macos")` или общие guard'ы без смены семантики).

## 6. Success criteria

- **SC-1** Джоба `macos` зелёная (полный функциональный сьют, 0 failed).
- **SC-2** Релизный macOS-артефакт: `codesign -v` проходит, smoke
  (`--version`, `install`, `index`, `search`) зелёный на раннере.
- **SC-3** Daemon lifecycle на macOS: start/stop/grace, janitor, watcher —
  зелёные (тесты `lifecycle`, `cleanup`, `watcher`, `mcp_registry`).
- **SC-4** Linux/Windows CI без регрессий; доки и конституция обновлены.

## 7. Risks

| Риск | Митигация |
|---|---|
| notify FSEvents: #937 (пропадают колбэки), #942 (дедлок второго watcher) | тест с несколькими проектами на macOS CI; мониторим upstream |
| Битые ad-hoc подписи после strip/lipo | `codesign -s - --force` + `codesign -v` gate |
| FSEvents-формы событий (burst/index_dir_ignored) | macOS-ветка бюджета 5 с; наблюдение в CI |
| Case-sensitive APFS (редкий) | fold как на Windows; ограничение в доке |
| Длинный cache-путь (>104) | FR-6 guard с понятной ошибкой |
| Intel/universal запросы | документированный out-of-scope + Tier 2 обоснование |
